//! [`RedisStreamsBroker`]: id_effect_messaging's [`MessageBroker`] on Redis Streams (feature
//! `streams`).
//!
//! A topic is a stream (its key is the topic name) and a consumer group a Redis consumer group:
//!
//! - publish: `XADD <topic> [MAXLEN ~ n] * id <id> [key <key>] h:<name> <value>… payload <bytes>`;
//! - subscribe: `XGROUP CREATE <topic> <group> 0|$ MKSTREAM` (an existing group is kept), then a
//!   dedicated connection pulls `XAUTOCLAIM` (idle pending entries: redeliveries, with their
//!   count from `XPENDING`) and else `XREADGROUP … COUNT <credit> BLOCK <ms> … >`, where the
//!   credit is the prefetch minus the deliveries still unsettled;
//! - ack: `XACK`; `Nack::Requeue`: `XCLAIM … IDLE <min idle> JUSTID`, so the next reclaim
//!   redelivers it without waiting; a dropped delivery stays pending and is reclaimed once idle
//!   for [`min_idle`](RedisStreamsConfig::min_idle);
//! - dead-letter (`Nack::DeadLetter`, a requeue of the `max_deliveries`-th delivery, or a
//!   reclaimed entry already delivered `max_deliveries` times): one Lua script `XACK`s the entry
//!   and, if the group still had it pending, `XADD`s [`Message::dead_lettered`] to the
//!   dead-letter stream.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Deferred, Effect, Never, Stream, fail, from_async, succeed, suspend};
use id_effect_messaging::{
  Acker, BrokerError, ConsumerGroup, DeadLetterReason, Delivery, Message, MessageBroker, MessageId,
  Nack, StartFrom, SubscribeOptions, Topic,
};
use redis::aio::ConnectionManager;
use redis::streams::{StreamAutoClaimReply, StreamId, StreamPendingCountReply, StreamReadReply};
use redis::{Cmd, Script};

use crate::RedisError;
use crate::client::RedisClient;

/// The entry field holding the [`MessageId`].
const FIELD_ID: &str = "id";
/// The entry field holding the message key, when it has one.
const FIELD_KEY: &str = "key";
/// The prefix of the entry fields holding the headers (`h:<name>`).
const HEADER_PREFIX: &str = "h:";
/// The entry field holding the payload.
const FIELD_PAYLOAD: &str = "payload";

/// Dead-letters an entry. KEYS: topic, dead-letter stream. ARGV: group, entry id, then the
/// `XADD` arguments after the key. Adds to the dead-letter stream only if the `XACK` acked it.
static DEAD_LETTER: LazyLock<Script> = LazyLock::new(|| {
  Script::new(
    r"
if redis.call('XACK', KEYS[1], ARGV[1], ARGV[2]) == 1 then
  redis.call('XADD', KEYS[2], unpack(ARGV, 3))
  return 1
end
return 0
",
  )
});

impl From<RedisError> for BrokerError {
  /// [`RedisError::Connection`] is [`BrokerError::Connection`], [`RedisError::Decode`]
  /// [`BrokerError::Decode`], anything else [`BrokerError::Backend`].
  fn from(e: RedisError) -> Self {
    match e {
      RedisError::Connection(m) => BrokerError::Connection(m),
      RedisError::Decode(m) => BrokerError::Decode(m),
      other => BrokerError::Backend(other.to_string()),
    }
  }
}

fn broker_error(e: redis::RedisError) -> BrokerError {
  RedisError::from(e).into()
}

/// How a [`RedisStreamsBroker`] consumes and trims its streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedisStreamsConfig {
  consumer: String,
  min_idle: Duration,
  block: Duration,
  max_len: Option<u64>,
}

impl Default for RedisStreamsConfig {
  /// A generated consumer name, [`DEFAULT_MIN_IDLE`](Self::DEFAULT_MIN_IDLE),
  /// [`DEFAULT_BLOCK`](Self::DEFAULT_BLOCK) and unbounded streams.
  fn default() -> Self {
    Self {
      consumer: format!("consumer-{}", MessageId::generate()),
      min_idle: Self::DEFAULT_MIN_IDLE,
      block: Self::DEFAULT_BLOCK,
      max_len: None,
    }
  }
}

impl RedisStreamsConfig {
  /// The default [`min_idle`](Self::min_idle).
  pub const DEFAULT_MIN_IDLE: Duration = Duration::from_secs(30);
  /// The default [`block`](Self::block).
  pub const DEFAULT_BLOCK: Duration = Duration::from_secs(2);

  /// The defaults ([`Default`]).
  pub fn new() -> Self {
    Self::default()
  }

  /// Read as the Redis consumer `name` in every group. Give each instance a stable name (its
  /// pod or host name), unique among the instances: the entries a consumer holds are reclaimed
  /// by the others only once idle, and the default name is new on every start, which leaves an
  /// empty consumer behind in each group (`XGROUP DELCONSUMER` removes it).
  #[must_use]
  pub fn consumer(mut self, name: impl Into<String>) -> Self {
    self.consumer = name.into();
    self
  }

  /// A pending entry idle (delivered and not settled) for `min_idle` is reclaimed and
  /// redelivered (`XAUTOCLAIM`): a delivery dropped unsettled, or held by a consumer that
  /// crashed. Keep it above the longest handler run, or a slow delivery is also handed to
  /// another consumer. Default [`DEFAULT_MIN_IDLE`](Self::DEFAULT_MIN_IDLE).
  #[must_use]
  pub fn min_idle(mut self, min_idle: Duration) -> Self {
    self.min_idle = min_idle;
    self
  }

  /// How long one `XREADGROUP` waits for new entries (at least 1 ms) before the subscription
  /// looks for idle entries again; also how long a subscription may take to notice
  /// [`close`](RedisStreamsBroker::close). Default [`DEFAULT_BLOCK`](Self::DEFAULT_BLOCK).
  #[must_use]
  pub fn block(mut self, block: Duration) -> Self {
    self.block = block.max(Duration::from_millis(1));
    self
  }

  /// Trim each stream to about `n` entries on every publish (`XADD … MAXLEN ~ n`). Entries are
  /// kept after every group acked them, so a stream grows until trimmed; trimming also drops
  /// entries a slow group has not read yet, or holds pending. Unbounded by default.
  #[must_use]
  pub fn max_len(mut self, n: u64) -> Self {
    self.max_len = Some(n);
    self
  }

  /// The consumer name.
  pub fn consumer_name(&self) -> &str {
    &self.consumer
  }

  /// The reclaim idle time.
  pub fn min_idle_time(&self) -> Duration {
    self.min_idle
  }

  /// The `XREADGROUP` block time.
  pub fn block_time(&self) -> Duration {
    self.block
  }

  /// The approximate stream length cap, if any.
  pub fn max_len_cap(&self) -> Option<u64> {
    self.max_len
  }

  fn min_idle_ms(&self) -> u64 {
    u64::try_from(self.min_idle.as_millis()).unwrap_or(u64::MAX)
  }

  fn block_ms(&self) -> u64 {
    u64::try_from(self.block.as_millis()).unwrap_or(u64::MAX)
  }
}

/// An entry held by a delivery of this broker: topic, group, entry id.
type HeldKey = (String, String, String);

struct Shared {
  redis: RedisClient,
  config: RedisStreamsConfig,
  closed: AtomicBool,
  /// Completed (and replaced) when a delivery is settled or the broker closed: a subscription
  /// out of prefetch credit waits on it.
  wake: Mutex<Deferred<(), Never>>,
  /// The entries handed out and not settled yet, so a reclaim does not hand them out twice.
  held: Mutex<HashSet<HeldKey>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
  fn wake(&self) {
    let waiters = std::mem::take(&mut *lock(&self.wake));
    waiters.try_succeed(());
  }

  fn is_closed(&self) -> bool {
    self.closed.load(Ordering::SeqCst)
  }
}

/// The `XADD` arguments after the stream key for `message`.
fn xadd_args(max_len: Option<u64>, message: &Message) -> Vec<Vec<u8>> {
  let mut args: Vec<Vec<u8>> = Vec::with_capacity(8 + 2 * message.headers().len());
  if let Some(n) = max_len {
    args.extend([
      b"MAXLEN".to_vec(),
      b"~".to_vec(),
      n.to_string().into_bytes(),
    ]);
  }
  args.push(b"*".to_vec());
  args.extend([FIELD_ID.into(), message.id().as_str().into()]);
  if let Some(key) = message.key() {
    args.extend([FIELD_KEY.into(), key.into()]);
  }
  for (name, value) in message.headers() {
    args.extend([
      format!("{HEADER_PREFIX}{name}").into_bytes(),
      value.clone().into_bytes(),
    ]);
  }
  args.extend([FIELD_PAYLOAD.into(), message.payload().to_vec()]);
  args
}

fn xadd(stream: &str, max_len: Option<u64>, message: &Message) -> Cmd {
  let mut cmd = redis::cmd("XADD");
  cmd.arg(stream);
  for arg in xadd_args(max_len, message) {
    cmd.arg(arg);
  }
  cmd
}

/// The message an entry carries. An entry not written by this broker reads leniently: the
/// entry id stands for a missing message id, a missing payload is empty.
fn message_of(entry: &StreamId) -> Message {
  let payload: Vec<u8> = entry.get(FIELD_PAYLOAD).unwrap_or_default();
  let id: String = entry.get(FIELD_ID).unwrap_or_else(|| entry.id.clone());
  let mut message = Message::new(payload).with_id(id);
  if let Some(key) = entry.get::<String>(FIELD_KEY) {
    message = message.with_key(key);
  }
  for field in entry.map.keys() {
    if let Some(name) = field.strip_prefix(HEADER_PREFIX)
      && let Some(value) = entry.get::<String>(field)
    {
      message = message.with_header(name, value);
    }
  }
  message
}

/// A [`MessageBroker`] on Redis Streams, over a [`RedisClient`]. Clones share the client and
/// the subscriptions' state. Needs Redis 6.2 or later (`XAUTOCLAIM`).
///
/// Publishing and settling use the client's pool; each subscription opens a dedicated
/// reconnecting connection for its blocking reads. Delivery is at least once: an unsettled
/// delivery (dropped, or held by a consumer that crashed) is redelivered once idle for
/// [`RedisStreamsConfig::min_idle`], so handlers must be idempotent (see id_effect_messaging's
/// `Inbox`). [`Delivery::attempt`] is the entry's delivery count in the group.
///
/// ```no_run
/// use id_effect::Effect;
/// use id_effect_messaging::{
///   BrokerError, ConsumerGroup, MessageBrokerExt, SubscribeOptions, Topic, ack_on_success,
/// };
/// use id_effect_redis::{RedisClient, RedisStreamsBroker, RedisStreamsConfig};
///
/// fn bill_orders(redis: RedisClient) -> Effect<(), BrokerError> {
///   let broker = RedisStreamsBroker::new(redis, RedisStreamsConfig::new().consumer("billing-1"));
///   let (orders, billing) = (Topic::new("orders").unwrap(), ConsumerGroup::new("billing").unwrap());
///   let options = SubscribeOptions::default().prefetch(32).concurrency(4);
///   broker.consume(&orders, &billing, options, ack_on_success(|delivery| {
///     println!("billing {}", delivery.message().id());
///     id_effect::succeed::<(), (), _>(())
///   }))
/// }
/// ```
#[derive(Clone)]
pub struct RedisStreamsBroker {
  shared: Arc<Shared>,
}

impl fmt::Debug for RedisStreamsBroker {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RedisStreamsBroker")
      .field("redis", &self.shared.redis)
      .field("config", &self.shared.config)
      .field("closed", &self.shared.is_closed())
      .finish()
  }
}

impl RedisStreamsBroker {
  /// A broker over `redis`, consuming as `config` says.
  pub fn new(redis: RedisClient, config: RedisStreamsConfig) -> Self {
    Self {
      shared: Arc::new(Shared {
        redis,
        config,
        closed: AtomicBool::new(false),
        wake: Mutex::new(Deferred::new()),
        held: Mutex::new(HashSet::new()),
      }),
    }
  }

  /// The client it runs on.
  pub fn client(&self) -> &RedisClient {
    &self.shared.redis
  }

  /// Its configuration.
  pub fn config(&self) -> &RedisStreamsConfig {
    &self.shared.config
  }

  /// Shuts this broker (and its clones) down: publish and subscribe fail with
  /// [`BrokerError::Closed`], and every subscription stream ends at its next pull, within
  /// [`RedisStreamsConfig::block`] for one waiting on Redis. Unsettled deliveries can still be
  /// settled. The streams and groups in Redis are kept.
  pub fn close(&self) {
    self.shared.closed.store(true, Ordering::SeqCst);
    self.shared.wake();
  }
}

impl MessageBroker for RedisStreamsBroker {
  fn publish(&self, topic: &Topic, message: Message) -> Effect<(), BrokerError> {
    let shared = Arc::clone(&self.shared);
    let cmd = xadd(topic.as_str(), shared.config.max_len, &message);
    suspend(move || {
      if shared.is_closed() {
        return fail(BrokerError::Closed);
      }
      shared
        .redis
        .query::<String>(cmd)
        .map(|_| ())
        .map_err(BrokerError::from)
    })
  }

  fn subscribe(
    &self,
    topic: &Topic,
    group: &ConsumerGroup,
    options: SubscribeOptions,
  ) -> Effect<Stream<Delivery, BrokerError>, BrokerError> {
    let shared = Arc::clone(&self.shared);
    let (topic, group) = (topic.clone(), group.clone());
    suspend(move || {
      if shared.is_closed() {
        return fail(BrokerError::Closed);
      }
      let dead_letter = match options.validate(&topic) {
        Ok(t) => t,
        Err(e) => return fail(e),
      };
      let client = shared.redis.redis().clone();
      let manager = shared
        .redis
        .config()
        .blocking_manager_config(shared.config.block);
      let start = match options.start {
        StartFrom::Earliest => "0",
        StartFrom::Latest => "$",
      };
      let (key, name) = (topic.as_str().to_owned(), group.as_str().to_owned());
      from_async(move || async move {
        let mut conn = ConnectionManager::new_with_config(client, manager)
          .await
          .map_err(broker_error)?;
        let created: Result<(), redis::RedisError> = redis::cmd("XGROUP")
          .arg("CREATE")
          .arg(&key)
          .arg(&name)
          .arg(start)
          .arg("MKSTREAM")
          .query_async(&mut conn)
          .await;
        match created {
          Err(e) if e.code() != Some("BUSYGROUP") => Err(broker_error(e)),
          _ => Ok(conn),
        }
      })
      .map(move |conn| {
        let sub = Arc::new(Sub {
          shared,
          topic,
          group,
          dead_letter,
          prefetch: options.prefetch,
          max_deliveries: options.max_deliveries,
          in_flight: AtomicUsize::new(0),
          conn,
          cursor: Mutex::new("0-0".to_owned()),
        });
        Stream::from_pull(move |max| pull(Arc::clone(&sub), max))
      })
    })
  }
}

/// One subscription.
struct Sub {
  shared: Arc<Shared>,
  topic: Topic,
  group: ConsumerGroup,
  dead_letter: Topic,
  prefetch: usize,
  max_deliveries: u32,
  /// Deliveries handed out and not settled yet.
  in_flight: AtomicUsize,
  /// The subscription's own connection, for its blocking reads.
  conn: ConnectionManager,
  /// Where the next `XAUTOCLAIM` scan of the pending entries starts.
  cursor: Mutex<String>,
}

impl Sub {
  fn held_key(&self, entry: &str) -> HeldKey {
    (
      self.topic.as_str().to_owned(),
      self.group.as_str().to_owned(),
      entry.to_owned(),
    )
  }

  fn is_held(&self, entry: &str) -> bool {
    lock(&self.shared.held).contains(&self.held_key(entry))
  }

  /// Hands `entry` out: it counts against the prefetch credit until settled or dropped.
  fn deliver(self: &Arc<Self>, entry: String, message: Message, attempt: u32) -> Delivery {
    lock(&self.shared.held).insert(self.held_key(&entry));
    self.in_flight.fetch_add(1, Ordering::SeqCst);
    let acker = StreamAcker {
      sub: Arc::clone(self),
      entry,
      message: message.clone(),
      attempt,
    };
    Delivery::new(
      message,
      self.topic.clone(),
      self.group.clone(),
      attempt,
      Box::new(acker),
    )
  }

  /// The dead-letter script for `entry`, `deliveries` times delivered.
  fn dead_letter_invocation(
    &self,
    entry: &str,
    message: Message,
    deliveries: u32,
    reason: DeadLetterReason,
  ) -> redis::ScriptInvocation<'static> {
    let dead = message.dead_lettered(&self.topic, &self.group, deliveries, reason);
    let mut invocation = DEAD_LETTER.prepare_invoke();
    invocation
      .key(self.topic.as_str())
      .key(self.dead_letter.as_str())
      .arg(self.group.as_str())
      .arg(entry);
    for arg in xadd_args(self.shared.config.max_len, &dead) {
      invocation.arg(arg);
    }
    invocation
  }

  /// Up to `want` idle pending entries, claimed for this consumer; the ones already delivered
  /// `max_deliveries` times are dead-lettered instead. Also whether the scan of the pending
  /// entries is complete.
  async fn reclaim(
    self: &Arc<Self>,
    conn: &mut ConnectionManager,
    want: usize,
  ) -> Result<(Vec<Delivery>, bool), BrokerError> {
    let config = &self.shared.config;
    let (key, group) = (self.topic.as_str(), self.group.as_str());
    let cursor = lock(&self.cursor).clone();
    let reply: StreamAutoClaimReply = redis::cmd("XAUTOCLAIM")
      .arg(key)
      .arg(group)
      .arg(&config.consumer)
      .arg(config.min_idle_ms())
      .arg(&cursor)
      .arg("COUNT")
      .arg(want)
      .query_async(conn)
      .await
      .map_err(broker_error)?;
    let complete = reply.next_stream_id == "0-0";
    *lock(&self.cursor) = reply.next_stream_id;
    // A delivery of this broker that outlived min_idle is still being handled: not again.
    let claimed: Vec<StreamId> = reply
      .claimed
      .into_iter()
      .filter(|e| !self.is_held(&e.id))
      .collect();
    if claimed.is_empty() {
      return Ok((Vec::new(), complete));
    }
    let mut pipe = redis::pipe();
    for entry in &claimed {
      pipe
        .cmd("XPENDING")
        .arg(key)
        .arg(group)
        .arg(&entry.id)
        .arg(&entry.id)
        .arg(1);
    }
    let counts: Vec<StreamPendingCountReply> =
      pipe.query_async(conn).await.map_err(broker_error)?;
    let mut out = Vec::with_capacity(claimed.len());
    for (entry, pending) in claimed.into_iter().zip(counts) {
      // Settled meanwhile by its previous holder.
      let Some(pending) = pending.ids.first() else {
        continue;
      };
      let count = u32::try_from(pending.times_delivered).unwrap_or(u32::MAX);
      let message = message_of(&entry);
      if count > self.max_deliveries {
        self
          .dead_letter_invocation(
            &entry.id,
            message,
            count - 1,
            DeadLetterReason::MaxDeliveries,
          )
          .invoke_async::<i64>(conn)
          .await
          .map_err(broker_error)?;
      } else {
        out.push(self.deliver(entry.id, message, count));
      }
    }
    Ok((out, complete))
  }

  /// Up to `want` new entries, waiting up to the block time for one when `block`.
  async fn read_new(
    self: &Arc<Self>,
    conn: &mut ConnectionManager,
    want: usize,
    block: bool,
  ) -> Result<Vec<Delivery>, BrokerError> {
    let config = &self.shared.config;
    let mut cmd = redis::cmd("XREADGROUP");
    cmd
      .arg("GROUP")
      .arg(self.group.as_str())
      .arg(&config.consumer)
      .arg("COUNT")
      .arg(want);
    if block {
      cmd.arg("BLOCK").arg(config.block_ms());
    }
    cmd.arg("STREAMS").arg(self.topic.as_str()).arg(">");
    let reply: Option<StreamReadReply> = cmd.query_async(conn).await.map_err(broker_error)?;
    Ok(
      reply
        .into_iter()
        .flat_map(|r| r.keys)
        .flat_map(|k| k.ids)
        .map(|entry| {
          let message = message_of(&entry);
          self.deliver(entry.id, message, 1)
        })
        .collect(),
    )
  }
}

/// One pull: up to `max` deliveries within the prefetch credit, waiting for credit and then for
/// entries; the end once the broker is closed.
fn pull(sub: Arc<Sub>, max: usize) -> Effect<Option<Vec<Delivery>>, BrokerError> {
  suspend(move || {
    let credit = {
      let wake = lock(&sub.shared.wake);
      if sub.shared.is_closed() {
        return succeed(None);
      }
      let credit = sub
        .prefetch
        .saturating_sub(sub.in_flight.load(Ordering::SeqCst));
      if credit == 0 {
        let waiter = wake.clone();
        drop(wake);
        return waiter
          .wait()
          .map_err(|never: Never| match never {})
          .flat_map(move |()| pull(sub, max));
      }
      credit
    };
    let want = credit.min(max.max(1));
    from_async(move || async move {
      let mut conn = sub.conn.clone();
      loop {
        if sub.shared.is_closed() {
          return Ok(None);
        }
        let (reclaimed, complete) = sub.reclaim(&mut conn, want).await?;
        if !reclaimed.is_empty() {
          return Ok(Some(reclaimed));
        }
        // Block only once every pending entry was looked at.
        let fresh = sub.read_new(&mut conn, want, complete).await?;
        if !fresh.is_empty() {
          return Ok(Some(fresh));
        }
      }
    })
  })
}

/// Settles one delivery. Dropped (settled or not) it frees its prefetch credit; an unsettled
/// entry stays pending in Redis until reclaimed.
struct StreamAcker {
  sub: Arc<Sub>,
  entry: String,
  message: Message,
  attempt: u32,
}

impl Drop for StreamAcker {
  fn drop(&mut self) {
    let key = self.sub.held_key(&self.entry);
    lock(&self.sub.shared.held).remove(&key);
    self.sub.in_flight.fetch_sub(1, Ordering::SeqCst);
    self.sub.shared.wake();
  }
}

impl StreamAcker {
  /// Runs `effect`, then drops the acker (freeing the credit), whatever the outcome.
  fn finish(self: Box<Self>, effect: Effect<(), RedisError>) -> Effect<(), BrokerError> {
    effect.map(move |()| drop(self)).map_err(BrokerError::from)
  }
}

impl Acker for StreamAcker {
  fn ack(self: Box<Self>) -> Effect<(), BrokerError> {
    let cmd = redis::cmd("XACK")
      .arg(self.sub.topic.as_str())
      .arg(self.sub.group.as_str())
      .arg(&self.entry)
      .clone();
    let ack = self.sub.shared.redis.query::<i64>(cmd).map(|_| ());
    self.finish(ack)
  }

  fn nack(self: Box<Self>, nack: Nack) -> Effect<(), BrokerError> {
    let reason = match nack {
      Nack::DeadLetter => Some(DeadLetterReason::Rejected),
      Nack::Requeue if self.attempt >= self.sub.max_deliveries => {
        Some(DeadLetterReason::MaxDeliveries)
      }
      Nack::Requeue => None,
    };
    let redis = self.sub.shared.redis.clone();
    let effect = match reason {
      Some(reason) => {
        let invocation =
          self
            .sub
            .dead_letter_invocation(&self.entry, self.message.clone(), self.attempt, reason);
        let mut conn = redis.connection();
        from_async(move || async move {
          invocation.invoke_async::<i64>(&mut conn).await?;
          Ok(())
        })
      }
      // Idle for min_idle already, without counting a delivery: the next reclaim takes it.
      None => {
        let config = &self.sub.shared.config;
        let cmd = redis::cmd("XCLAIM")
          .arg(self.sub.topic.as_str())
          .arg(self.sub.group.as_str())
          .arg(&config.consumer)
          .arg(0)
          .arg(&self.entry)
          .arg("IDLE")
          .arg(config.min_idle_ms())
          .arg("JUSTID")
          .clone();
        redis.query::<Vec<String>>(cmd).map(|_| ())
      }
    };
    self.finish(effect)
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use redis::Value;

  use super::*;

  fn entry(id: &str, fields: &[(&str, &[u8])]) -> StreamId {
    StreamId {
      id: id.to_owned(),
      map: fields
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Value::BulkString(v.to_vec())))
        .collect::<HashMap<_, _>>(),
      ..StreamId::default()
    }
  }

  #[test]
  fn messages_round_trip_through_entry_fields() {
    let message = Message::new(vec![0xff, 0x00])
      .with_id("m-1")
      .with_key("acct-1")
      .with_header("content-type", "application/json");
    let args = xadd_args(Some(1000), &message);
    assert_eq!(&args[..4], [&b"MAXLEN"[..], b"~", b"1000", b"*"]);
    let fields: Vec<(String, Vec<u8>)> = args[4..]
      .chunks(2)
      .map(|kv| (String::from_utf8(kv[0].clone()).unwrap(), kv[1].clone()))
      .collect();
    let borrowed: Vec<(&str, &[u8])> = fields
      .iter()
      .map(|(k, v)| (k.as_str(), v.as_slice()))
      .collect();
    assert_eq!(message_of(&entry("1-0", &borrowed)), message);
    assert_eq!(xadd_args(None, &Message::new("p").with_id("x"))[0], b"*");
  }

  #[test]
  fn foreign_entries_read_leniently() {
    let m = message_of(&entry("7-1", &[("other", b"x")]));
    assert_eq!(
      (m.id().as_str(), m.key(), m.payload()),
      ("7-1", None, &b""[..])
    );
    assert!(m.headers().is_empty());
  }

  #[test]
  fn config_builder_and_error_mapping() {
    let config = RedisStreamsConfig::new()
      .consumer("billing-1")
      .min_idle(Duration::from_millis(1500))
      .block(Duration::ZERO)
      .max_len(10);
    assert_eq!(config.consumer_name(), "billing-1");
    assert_eq!(
      (config.min_idle_time(), config.min_idle_ms()),
      (Duration::from_millis(1500), 1500)
    );
    assert_eq!(
      (config.block_time(), config.block_ms()),
      (Duration::from_millis(1), 1)
    );
    assert_eq!(config.max_len_cap(), Some(10));
    let default = RedisStreamsConfig::default();
    assert!(default.consumer_name().starts_with("consumer-"));
    assert_ne!(
      default.consumer_name(),
      RedisStreamsConfig::default().consumer_name()
    );
    assert_eq!(default.max_len_cap(), None);
    assert_eq!(
      BrokerError::from(RedisError::Connection("down".into())),
      BrokerError::Connection("down".into())
    );
    assert_eq!(
      BrokerError::from(RedisError::Decode("bad".into())),
      BrokerError::Decode("bad".into())
    );
    assert!(matches!(
      BrokerError::from(RedisError::Command("WRONGTYPE".into())),
      BrokerError::Backend(m) if m.contains("WRONGTYPE")
    ));
  }
}
