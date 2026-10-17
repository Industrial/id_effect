//! The transactional outbox: [`Outbox::stage`] writes a message to the table `id_effect_outbox`
//! with the caller's [`SqlTransaction`], so it is published if and only if the caller's work
//! commits; a [`Relay`](crate::Relay) publishes the committed rows through an [`OutboxStore`]:
//! [`PgOutbox`] in PostgreSQL, [`MemoryOutbox`] in memory.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Effect, UtcDateTime, WallClock, fail, succeed, suspend};
use id_effect_sql::{SqlClient, SqlClientService, SqlError, SqlRow, SqlTransaction, params};

use crate::Topic;
use crate::broker::Message;
use crate::schema::{SCHEMA, statements};

/// The longest `last_error` kept, in bytes.
const MAX_ERROR_LEN: usize = 4096;

pub(crate) const STAGE: &str = "INSERT INTO id_effect_outbox (topic, message_id, message_key, \
  headers, payload, available_at, inserted_at) VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING id";

/// Claim: the attempt is counted here, so a relay that crashes mid-publish still consumes it. An
/// expired lease is claimable again (the crashed relay's rows).
pub(crate) const CLAIM: &str = "WITH next AS (SELECT id FROM id_effect_outbox \
  WHERE sent_at IS NULL AND dead_at IS NULL AND available_at <= $1 \
  AND (locked_until IS NULL OR locked_until < $1) \
  ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED) \
  UPDATE id_effect_outbox o SET attempts = o.attempts + 1, locked_by = $3, locked_until = $4 \
  FROM next WHERE o.id = next.id \
  RETURNING o.id, o.topic, o.message_id, o.message_key, o.headers, o.payload, o.attempts";

pub(crate) const MARK_SENT: &str = "UPDATE id_effect_outbox SET sent_at = $3, last_error = NULL, \
  locked_by = NULL, locked_until = NULL WHERE id = $1 AND locked_by = $2 AND sent_at IS NULL";

pub(crate) const RETRY: &str = "UPDATE id_effect_outbox SET available_at = $3, last_error = $4, \
  locked_by = NULL, locked_until = NULL WHERE id = $1 AND locked_by = $2 AND sent_at IS NULL";

pub(crate) const DEAD: &str = "UPDATE id_effect_outbox SET dead_at = $3, last_error = $4, \
  locked_by = NULL, locked_until = NULL WHERE id = $1 AND locked_by = $2 AND sent_at IS NULL";

pub(crate) const RELEASE: &str = "UPDATE id_effect_outbox SET \
  attempts = GREATEST(attempts - 1, 0), locked_by = NULL, locked_until = NULL \
  WHERE locked_by = $1 AND sent_at IS NULL AND dead_at IS NULL";

pub(crate) const PRUNE: &str = "DELETE FROM id_effect_outbox WHERE sent_at < $1";

/// The id of an outbox row (the `id` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutboxId(pub i64);

impl fmt::Display for OutboxId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.0)
  }
}

/// An outbox row a relay claimed: publish `message` to `topic`, then settle the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
  /// The row.
  pub id: OutboxId,
  /// Where to publish.
  pub topic: Topic,
  /// What to publish, with the [`MessageId`](crate::MessageId) it was staged with.
  pub message: Message,
  /// This attempt's number, from 1 (counted at the claim).
  pub attempt: u32,
}

/// An outbox store's failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutboxError {
  /// The database failed (or a row did not decode).
  Sql(SqlError),
  /// A row holds what is not a valid message (a topic name or headers written by hand).
  Decode(String),
}

impl fmt::Display for OutboxError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      OutboxError::Sql(e) => write!(f, "outbox: {e}"),
      OutboxError::Decode(m) => write!(f, "outbox row: {m}"),
    }
  }
}

impl std::error::Error for OutboxError {}

impl From<SqlError> for OutboxError {
  fn from(e: SqlError) -> Self {
    OutboxError::Sql(e)
  }
}

/// Where a [`Relay`](crate::Relay) finds the staged messages. Every time is passed in from the
/// relay's [`WallClock`]; `relay` is the relay's id, the owner of a lease.
///
/// - [`claim`](Self::claim) leases up to `limit` publishable rows (not sent, not dead, available,
///   not leased or with an expired lease) to `relay` until `now + lease`, oldest first, counting
///   an attempt; two relays never hold the same row.
/// - [`mark_sent`](Self::mark_sent), [`retry_at`](Self::retry_at) and [`dead`](Self::dead) settle
///   a claimed row and are fenced on the lease: they apply (`true`) only while `relay` still
///   holds the row.
pub trait OutboxStore: Send + Sync + 'static {
  /// Leases up to `limit` rows to `relay` until `now + lease`, oldest first.
  fn claim(
    &self,
    relay: &str,
    limit: u32,
    now: UtcDateTime,
    lease: Duration,
  ) -> Effect<Vec<Pending>, OutboxError>;

  /// The row was published: it is kept (sent) until [`prune`](Self::prune).
  fn mark_sent(&self, id: OutboxId, relay: &str, now: UtcDateTime) -> Effect<bool, OutboxError>;

  /// Publishing failed with `error`: claimable again from `at`.
  fn retry_at(
    &self,
    id: OutboxId,
    relay: &str,
    at: UtcDateTime,
    error: &str,
  ) -> Effect<bool, OutboxError>;

  /// Publishing failed with `error` and the retry schedule is done: the row is dead (kept,
  /// never claimed again).
  fn dead(
    &self,
    id: OutboxId,
    relay: &str,
    now: UtcDateTime,
    error: &str,
  ) -> Effect<bool, OutboxError>;

  /// Gives back every unsettled row `relay` holds, refunding the attempt (a relay stopping or
  /// abandoning the rest of a batch). Returns how many.
  fn release(&self, relay: &str) -> Effect<u64, OutboxError>;

  /// Deletes the rows sent before `sent_before`. Returns how many.
  fn prune(&self, sent_before: UtcDateTime) -> Effect<u64, OutboxError>;
}

/// `at + delay`, saturating at the latest representable instant.
pub(crate) fn later(at: UtcDateTime, delay: Duration) -> UtcDateTime {
  UtcDateTime(at.0.checked_add(delay).unwrap_or(jiff::Timestamp::MAX))
}

/// `at - delay`, saturating at the earliest representable instant.
pub(crate) fn earlier(at: UtcDateTime, delay: Duration) -> UtcDateTime {
  UtcDateTime(at.0.checked_sub(delay).unwrap_or(jiff::Timestamp::MIN))
}

/// `error` cut to at most [`MAX_ERROR_LEN`] bytes, on a character boundary.
fn truncate_error(error: &str) -> String {
  let mut end = error.len().min(MAX_ERROR_LEN);
  while !error.is_char_boundary(end) {
    end -= 1;
  }
  error[..end].to_owned()
}

fn headers_json(message: &Message) -> serde_json::Value {
  serde_json::Value::Object(
    message
      .headers()
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect(),
  )
}

fn headers_of(json: serde_json::Value) -> Result<BTreeMap<String, String>, OutboxError> {
  let serde_json::Value::Object(map) = json else {
    return Err(OutboxError::Decode("headers are not a JSON object".into()));
  };
  map
    .into_iter()
    .map(|(k, v)| match v {
      serde_json::Value::String(s) => Ok((k, s)),
      other => Err(OutboxError::Decode(format!(
        "header `{k}` is not a string: {other}"
      ))),
    })
    .collect()
}

fn decode_pending(row: &SqlRow) -> Result<Pending, OutboxError> {
  let topic = row.get::<String>("topic")?;
  let topic =
    Topic::new(topic.clone()).map_err(|e| OutboxError::Decode(format!("topic `{topic}`: {e}")))?;
  let mut message =
    Message::new(row.get::<Vec<u8>>("payload")?).with_id(row.get::<String>("message_id")?);
  if let Some(key) = row.get::<Option<String>>("message_key")? {
    message = message.with_key(key);
  }
  for (name, value) in headers_of(row.get::<serde_json::Value>("headers")?)? {
    message = message.with_header(name, value);
  }
  let attempts = row.get::<i32>("attempts")?;
  Ok(Pending {
    id: OutboxId(row.get("id")?),
    topic,
    message,
    attempt: u32::try_from(attempts).unwrap_or(0),
  })
}

fn ready<A: Send + 'static, E: Send + 'static>(r: Result<A, E>) -> Effect<A, E> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// Stages messages in the outbox table with the caller's transaction. Cheap to clone; it holds
/// only the [`WallClock`] the row times are read from.
#[derive(Clone, Debug)]
pub struct Outbox {
  clock: WallClock,
}

impl Default for Outbox {
  fn default() -> Self {
    Self::new()
  }
}

impl Outbox {
  /// An outbox on the system clock.
  pub fn new() -> Self {
    Self {
      clock: WallClock::system(),
    }
  }

  /// Reads the row times from `clock` (bind the one the relays read).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// Writes `message` for `topic` to `id_effect_outbox` with `tx`: the row commits or rolls back
  /// with the caller's work, and a relay publishes it (at least once) after the commit, with the
  /// message's id, key, headers and payload. Needs only the table, no relay or broker.
  pub fn stage(
    &self,
    tx: &Arc<dyn SqlTransaction>,
    topic: &Topic,
    message: Message,
  ) -> Effect<OutboxId, SqlError> {
    let tx = Arc::clone(tx);
    let topic = topic.clone();
    self.clock.now().flat_map(move |now: UtcDateTime| {
      let params = params![
        topic.as_str(),
        message.id().as_str(),
        message.key().map(str::to_owned),
        headers_json(&message),
        message.payload().to_vec(),
        now.0,
      ];
      tx.query(STAGE, &params)
        .flat_map(|rows| match rows.first() {
          Some(row) => ready(row.get::<i64>("id").map(OutboxId)),
          None => fail(SqlError::Decode("outbox insert returned no id".to_owned())),
        })
    })
  }
}

/// [`OutboxStore`] in the PostgreSQL table `id_effect_outbox`, over any [`SqlClient`]
/// (`id_effect_sql::pg::PgSqlClient` in production). Clones share the client.
///
/// Relays claim with `FOR UPDATE SKIP LOCKED` on a partial index of the unsent rows, and the
/// claim leases the rows (`locked_by`, `locked_until`), so two relays never publish the same row
/// at once and a crashed relay's rows are claimed again once the lease expires. Create the table
/// with [`migrator`](crate::migrator) or [`ensure_schema`](Self::ensure_schema).
#[derive(Clone)]
pub struct PgOutbox {
  client: SqlClientService,
}

impl fmt::Debug for PgOutbox {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PgOutbox")
      .field("table", &"id_effect_outbox")
      .finish_non_exhaustive()
  }
}

impl PgOutbox {
  /// A store over `client`.
  pub fn new(client: impl SqlClient) -> Self {
    Self::from_service(SqlClientService::new(client))
  }

  /// A store over the client capability `client` (shares the client and its pool).
  pub fn from_service(client: SqlClientService) -> Self {
    Self { client }
  }

  /// Create `id_effect_outbox`, `id_effect_inbox` and their indexes unless they exist: the
  /// statements of `migrations/0001_id_effect_messaging.up.sql`, one at a time. Safe to run at
  /// every start; two instances starting at the same moment may race on `CREATE TABLE` (use
  /// [`migrator`](crate::migrator), which takes a lock, when that matters).
  pub fn ensure_schema(&self) -> Effect<(), SqlError> {
    let client = self.client.clone();
    statements(SCHEMA)
      .into_iter()
      .fold(succeed(()), move |done, sql| {
        let client = client.clone();
        done.flat_map(move |()| client.execute(&sql, &[]).map(|_| ()))
      })
  }

  fn execute(
    &self,
    sql: &'static str,
    params: Vec<id_effect_sql::SqlValue>,
  ) -> Effect<u64, OutboxError> {
    self.client.execute(sql, &params).map_err(OutboxError::Sql)
  }
}

impl OutboxStore for PgOutbox {
  fn claim(
    &self,
    relay: &str,
    limit: u32,
    now: UtcDateTime,
    lease: Duration,
  ) -> Effect<Vec<Pending>, OutboxError> {
    let params = params![now.0, i64::from(limit), relay, later(now, lease).0];
    self
      .client
      .query(CLAIM, &params)
      .map_err(OutboxError::Sql)
      .flat_map(|rows| {
        let claimed = rows
          .iter()
          .map(decode_pending)
          .collect::<Result<Vec<_>, _>>();
        ready(claimed.map(|mut rows| {
          rows.sort_by_key(|p| p.id);
          rows
        }))
      })
  }

  fn mark_sent(&self, id: OutboxId, relay: &str, now: UtcDateTime) -> Effect<bool, OutboxError> {
    self
      .execute(MARK_SENT, params![id.0, relay, now.0])
      .map(|n| n > 0)
  }

  fn retry_at(
    &self,
    id: OutboxId,
    relay: &str,
    at: UtcDateTime,
    error: &str,
  ) -> Effect<bool, OutboxError> {
    self
      .execute(RETRY, params![id.0, relay, at.0, truncate_error(error)])
      .map(|n| n > 0)
  }

  fn dead(
    &self,
    id: OutboxId,
    relay: &str,
    now: UtcDateTime,
    error: &str,
  ) -> Effect<bool, OutboxError> {
    self
      .execute(DEAD, params![id.0, relay, now.0, truncate_error(error)])
      .map(|n| n > 0)
  }

  fn release(&self, relay: &str) -> Effect<u64, OutboxError> {
    self.execute(RELEASE, params![relay])
  }

  fn prune(&self, sent_before: UtcDateTime) -> Effect<u64, OutboxError> {
    self.execute(PRUNE, params![sent_before.0])
  }
}

/// One row of a [`MemoryOutbox`].
#[derive(Clone, Debug)]
struct Row {
  id: OutboxId,
  topic: Topic,
  message: Message,
  attempts: u32,
  available_at: UtcDateTime,
  sent_at: Option<UtcDateTime>,
  dead_at: Option<UtcDateTime>,
  locked_by: Option<String>,
  locked_until: Option<UtcDateTime>,
  last_error: Option<String>,
}

impl Row {
  fn unsettled(&self) -> bool {
    self.sent_at.is_none() && self.dead_at.is_none()
  }
}

#[derive(Debug, Default)]
struct MemoryState {
  next: i64,
  rows: Vec<Row>,
}

/// [`OutboxStore`] in memory, for tests and single-process development: the same claiming,
/// leasing and fencing as [`PgOutbox`], without a database. There is no transaction to stage
/// with, so [`stage`](Self::stage) writes at once. Clones share the rows.
#[derive(Clone, Debug)]
pub struct MemoryOutbox {
  state: Arc<Mutex<MemoryState>>,
  clock: WallClock,
}

impl Default for MemoryOutbox {
  fn default() -> Self {
    Self::new()
  }
}

impl MemoryOutbox {
  /// An empty outbox on the system clock.
  pub fn new() -> Self {
    Self {
      state: Arc::default(),
      clock: WallClock::system(),
    }
  }

  /// Reads the staging time from `clock` (bind the one the relays read).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  fn lock(&self) -> MutexGuard<'_, MemoryState> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Stages `message` for `topic`, publishable at once.
  pub fn stage(&self, topic: &Topic, message: Message) -> Effect<OutboxId, OutboxError> {
    let (state, topic) = (Arc::clone(&self.state), topic.clone());
    self.clock.now().map(move |now| {
      let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
      st.next += 1;
      let id = OutboxId(st.next);
      st.rows.push(Row {
        id,
        topic,
        message,
        attempts: 0,
        available_at: now,
        sent_at: None,
        dead_at: None,
        locked_by: None,
        locked_until: None,
        last_error: None,
      });
      id
    })
  }

  /// How many rows are neither sent nor dead.
  pub fn pending(&self) -> usize {
    self.lock().rows.iter().filter(|r| r.unsettled()).count()
  }

  /// How many rows are sent (and not pruned).
  pub fn sent(&self) -> usize {
    self
      .lock()
      .rows
      .iter()
      .filter(|r| r.sent_at.is_some())
      .count()
  }

  /// The dead rows, with their last error.
  pub fn dead_rows(&self) -> Vec<(OutboxId, String)> {
    self
      .lock()
      .rows
      .iter()
      .filter(|r| r.dead_at.is_some())
      .map(|r| (r.id, r.last_error.clone().unwrap_or_default()))
      .collect()
  }

  /// The last publish error of row `id`, if any.
  pub fn last_error(&self, id: OutboxId) -> Option<String> {
    self
      .lock()
      .rows
      .iter()
      .find(|r| r.id == id)
      .and_then(|r| r.last_error.clone())
  }

  /// `f` over the row `id` while `relay` holds it unsettled; `false` when it does not.
  fn settle(
    &self,
    id: OutboxId,
    relay: &str,
    f: impl FnOnce(&mut Row) + Send + 'static,
  ) -> Effect<bool, OutboxError> {
    let (state, relay) = (Arc::clone(&self.state), relay.to_owned());
    suspend(move || {
      let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
      let row = st
        .rows
        .iter_mut()
        .find(|r| r.id == id && r.unsettled() && r.locked_by.as_deref() == Some(&relay));
      succeed(
        row
          .map(|r| {
            f(r);
            r.locked_by = None;
            r.locked_until = None;
          })
          .is_some(),
      )
    })
  }
}

impl OutboxStore for MemoryOutbox {
  fn claim(
    &self,
    relay: &str,
    limit: u32,
    now: UtcDateTime,
    lease: Duration,
  ) -> Effect<Vec<Pending>, OutboxError> {
    let (state, relay) = (Arc::clone(&self.state), relay.to_owned());
    suspend(move || {
      let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
      let until = later(now, lease);
      let claimed = st
        .rows
        .iter_mut()
        .filter(|r| {
          r.unsettled() && r.available_at <= now && r.locked_until.is_none_or(|t| t < now)
        })
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .map(|r| {
          r.attempts += 1;
          r.locked_by = Some(relay.clone());
          r.locked_until = Some(until);
          Pending {
            id: r.id,
            topic: r.topic.clone(),
            message: r.message.clone(),
            attempt: r.attempts,
          }
        })
        .collect();
      succeed(claimed)
    })
  }

  fn mark_sent(&self, id: OutboxId, relay: &str, now: UtcDateTime) -> Effect<bool, OutboxError> {
    self.settle(id, relay, move |r| {
      r.sent_at = Some(now);
      r.last_error = None;
    })
  }

  fn retry_at(
    &self,
    id: OutboxId,
    relay: &str,
    at: UtcDateTime,
    error: &str,
  ) -> Effect<bool, OutboxError> {
    let error = truncate_error(error);
    self.settle(id, relay, move |r| {
      r.available_at = at;
      r.last_error = Some(error);
    })
  }

  fn dead(
    &self,
    id: OutboxId,
    relay: &str,
    now: UtcDateTime,
    error: &str,
  ) -> Effect<bool, OutboxError> {
    let error = truncate_error(error);
    self.settle(id, relay, move |r| {
      r.dead_at = Some(now);
      r.last_error = Some(error);
    })
  }

  fn release(&self, relay: &str) -> Effect<u64, OutboxError> {
    let (state, relay) = (Arc::clone(&self.state), relay.to_owned());
    suspend(move || {
      let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
      let mut released = 0;
      for r in st
        .rows
        .iter_mut()
        .filter(|r| r.unsettled() && r.locked_by.as_deref() == Some(&relay))
      {
        r.attempts = r.attempts.saturating_sub(1);
        r.locked_by = None;
        r.locked_until = None;
        released += 1;
      }
      succeed(released)
    })
  }

  fn prune(&self, sent_before: UtcDateTime) -> Effect<u64, OutboxError> {
    let state = Arc::clone(&self.state);
    suspend(move || {
      let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
      let before = st.rows.len();
      st.rows
        .retain(|r| r.sent_at.is_none_or(|t| t >= sent_before));
      succeed((before - st.rows.len()) as u64)
    })
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};
  use id_effect_sql::{SqlValue, TestSqlClient};

  use super::*;

  fn run<A: Send + 'static, E: fmt::Debug + Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn t(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn topic(name: &str) -> Topic {
    Topic::new(name).unwrap()
  }

  fn row(id: i64, topic: &str, headers: serde_json::Value) -> SqlRow {
    SqlRow::from_pairs([
      ("id", SqlValue::I64(id)),
      ("topic", SqlValue::Text(topic.into())),
      ("message_id", SqlValue::Text(format!("m-{id}"))),
      ("message_key", SqlValue::Text("acct-1".into())),
      ("headers", SqlValue::Json(headers)),
      ("payload", SqlValue::Bytes(b"hi".to_vec())),
      ("attempts", SqlValue::I32(2)),
    ])
  }

  #[test]
  fn stage_binds_the_message_with_the_callers_transaction() {
    let client = TestSqlClient::new();
    run(client.connect()).unwrap();
    client.script_query(STAGE, vec![SqlRow::from_pairs([("id", SqlValue::I64(9))])]);
    let clock = WallClock::anchored(std::time::Instant::now(), t(100));
    let outbox = Outbox::default().with_clock(clock);
    let tx = run(client.begin()).unwrap();
    let message = Message::new("hi")
      .with_id("m-1")
      .with_key("acct-1")
      .with_header("trace", "abc");
    let id = run(outbox.stage(&tx, &topic("orders"), message)).unwrap();
    assert_eq!(id, OutboxId(9));
    assert!(
      client.recorded_queries().is_empty(),
      "not before the commit"
    );
    run(tx.commit()).unwrap();
    let (sql, params) = &client.recorded_queries()[0];
    assert_eq!(sql, STAGE);
    assert_eq!(params[0], SqlValue::Text("orders".into()));
    assert_eq!(params[1], SqlValue::Text("m-1".into()));
    assert_eq!(params[2], SqlValue::Text("acct-1".into()));
    assert_eq!(
      params[3],
      SqlValue::Json(serde_json::json!({"trace": "abc"}))
    );
    assert_eq!(params[4], SqlValue::Bytes(b"hi".to_vec()));
    assert!(
      matches!(&params[5], SqlValue::Timestamp(at) if at.as_second() == 100),
      "the clock's time: {:?}",
      params[5]
    );

    // A rolled-back stage records nothing; an insert returning no row is an error.
    let tx = run(client.begin()).unwrap();
    client.script_query(STAGE, vec![]);
    let none = run(outbox.stage(&tx, &topic("orders"), Message::new("x")));
    assert!(matches!(none, Err(SqlError::Decode(_))));
    run(tx.rollback()).unwrap();
    assert_eq!(client.recorded_queries().len(), 1);
  }

  #[test]
  fn claim_decodes_rows_in_id_order_and_binds_the_lease() {
    let client = TestSqlClient::new();
    run(client.connect()).unwrap();
    let store = PgOutbox::new(client.clone());
    client.script_query(
      CLAIM,
      vec![
        row(7, "orders", serde_json::json!({"a": "1"})),
        row(3, "orders", serde_json::json!({})),
      ],
    );
    let claimed = run(store.claim("r1", 10, t(5), Duration::from_secs(30))).unwrap();
    assert_eq!(claimed.iter().map(|p| p.id.0).collect::<Vec<_>>(), [3, 7]);
    let p = &claimed[1];
    assert_eq!((p.topic.as_str(), p.attempt), ("orders", 2));
    assert_eq!(p.message.id().as_str(), "m-7");
    assert_eq!(p.message.key(), Some("acct-1"));
    assert_eq!(p.message.header("a"), Some("1"));
    assert_eq!(p.message.payload(), b"hi");
    let (_, params) = &client.recorded_queries()[0];
    assert_eq!(params[0], SqlValue::Timestamp(t(5).0));
    assert_eq!(params[1], SqlValue::I64(10));
    assert_eq!(params[2], SqlValue::Text("r1".into()));
    assert_eq!(params[3], SqlValue::Timestamp(t(35).0));
  }

  #[test]
  fn bad_rows_are_decode_errors() {
    let client = TestSqlClient::new();
    run(client.connect()).unwrap();
    let store = PgOutbox::new(client.clone());
    for (topic, headers) in [
      ("a b", serde_json::json!({})),
      ("orders", serde_json::json!([])),
      ("orders", serde_json::json!({"n": 1})),
    ] {
      client.script_query(CLAIM, vec![row(1, topic, headers)]);
      let r = run(store.claim("r", 1, t(1), Duration::from_secs(1)));
      assert!(matches!(r, Err(OutboxError::Decode(_))), "{r:?}");
    }
    assert!(
      OutboxError::Decode("x".into())
        .to_string()
        .contains("outbox row")
    );
    assert!(
      OutboxError::from(SqlError::NotConnected)
        .to_string()
        .starts_with("outbox:")
    );
  }

  #[test]
  fn settlements_are_fenced_and_bind_their_times() {
    let client = TestSqlClient::new();
    run(client.connect()).unwrap();
    let store = PgOutbox::new(client.clone());
    assert_eq!(run(store.mark_sent(OutboxId(1), "r", t(1))), Ok(false));
    client.script_execute(MARK_SENT, 1);
    assert_eq!(run(store.mark_sent(OutboxId(1), "r", t(1))), Ok(true));
    client.script_execute(RETRY, 1);
    let long = "é".repeat(3000);
    assert_eq!(run(store.retry_at(OutboxId(1), "r", t(9), &long)), Ok(true));
    let (_, params) = client.recorded_executes().pop().unwrap();
    assert_eq!(params[2], SqlValue::Timestamp(t(9).0));
    assert!(matches!(&params[3], SqlValue::Text(e) if e.len() <= MAX_ERROR_LEN && e.len() > 4000));
    client.script_execute(DEAD, 1);
    assert_eq!(run(store.dead(OutboxId(1), "r", t(2), "no")), Ok(true));
    for (sql, eff) in [(RELEASE, store.release("r")), (PRUNE, store.prune(t(1)))] {
      client.script_execute(sql, 4);
      assert_eq!(run(eff), Ok(4), "{sql}");
    }
    assert_eq!(
      format!("{store:?}"),
      "PgOutbox { table: \"id_effect_outbox\", .. }"
    );
  }

  #[test]
  fn ensure_schema_runs_the_migration_statements() {
    let client = TestSqlClient::new();
    run(client.connect()).unwrap();
    run(PgOutbox::new(client.clone()).ensure_schema()).unwrap();
    assert_eq!(client.recorded_executes().len(), 5);
  }

  #[test]
  fn memory_outbox_leases_fences_and_prunes() {
    let clock = WallClock::anchored(std::time::Instant::now(), t(99));
    let outbox = MemoryOutbox::new().with_clock(clock);
    let a = run(outbox.stage(&topic("orders"), Message::new("a"))).unwrap();
    let b = run(outbox.stage(&topic("orders"), Message::new("b"))).unwrap();
    assert_eq!((a, b), (OutboxId(1), OutboxId(2)));
    let lease = Duration::from_secs(30);
    let first = run(outbox.claim("r1", 1, t(100), lease)).unwrap();
    assert_eq!((first[0].id, first[0].attempt), (a, 1));
    // r2 sees only the unleased row; r1's lease fences r2 off row a.
    let second = run(outbox.claim("r2", 10, t(101), lease)).unwrap();
    assert_eq!(second.iter().map(|p| p.id).collect::<Vec<_>>(), [b]);
    assert_eq!(run(outbox.mark_sent(a, "r2", t(101))), Ok(false));
    // After the lease, r2 claims row a again (the second attempt).
    let again = run(outbox.claim("r2", 10, t(131), lease)).unwrap();
    assert_eq!((again[0].id, again[0].attempt), (a, 2));
    assert_eq!(
      run(outbox.mark_sent(a, "r1", t(131))),
      Ok(false),
      "r1 lost it"
    );
    assert_eq!(run(outbox.mark_sent(a, "r2", t(131))), Ok(true));
    assert_eq!(run(outbox.retry_at(b, "r2", t(200), "down")), Ok(true));
    assert_eq!(outbox.last_error(b).as_deref(), Some("down"));
    assert!(
      run(outbox.claim("r3", 10, t(150), lease))
        .unwrap()
        .is_empty(),
      "not yet"
    );
    let late = run(outbox.claim("r3", 10, t(200), lease)).unwrap();
    assert_eq!(late[0].attempt, 2);
    assert_eq!(run(outbox.release("r3")), Ok(1));
    let refunded = run(outbox.claim("r3", 10, t(200), lease)).unwrap();
    assert_eq!(refunded[0].attempt, 2, "the released attempt was refunded");
    assert_eq!(run(outbox.dead(b, "r3", t(200), "gave up")), Ok(true));
    assert_eq!(outbox.dead_rows(), [(b, "gave up".to_owned())]);
    assert_eq!((outbox.pending(), outbox.sent()), (0, 1));
    assert_eq!(run(outbox.prune(t(131))), Ok(0));
    assert_eq!(run(outbox.prune(t(132))), Ok(1));
    assert_eq!(outbox.sent(), 0);
    assert_eq!(earlier(t(10), Duration::from_secs(4)), t(6));
  }
}
