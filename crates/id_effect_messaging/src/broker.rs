//! The broker-agnostic API: [`Message`], [`ConsumerGroup`], [`SubscribeOptions`], [`BrokerError`]
//! and the [`MessageBroker`] trait every broker implements, with typed sugar in
//! [`MessageBrokerExt`] and the bounded-concurrency consumer loop [`consume`].

use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect::schema::{
  HasSchema, ParseErrors, Unknown, serde_json_from_unknown, unknown_from_serde_json,
};
use id_effect::{Effect, Stream};

use crate::delivery::{DeadLetterReason, Delivery};
use crate::{Topic, TopicError, validate_name};

/// The header names brokers set on the messages they carry.
pub mod headers {
  /// The payload's media type; [`Message::encode`](super::Message::encode) sets
  /// [`JSON`].
  pub const CONTENT_TYPE: &str = "content-type";
  /// The [`CONTENT_TYPE`] of a schema-encoded payload.
  pub const JSON: &str = "application/json";
  /// On a dead-lettered message: the topic it was consumed from.
  pub const DEAD_LETTER_TOPIC: &str = "x-dead-letter-topic";
  /// On a dead-lettered message: the consumer group that gave up on it.
  pub const DEAD_LETTER_GROUP: &str = "x-dead-letter-group";
  /// On a dead-lettered message: why, a [`DeadLetterReason`](crate::DeadLetterReason) as text.
  pub const DEAD_LETTER_REASON: &str = "x-dead-letter-reason";
  /// On a dead-lettered message: how many times the group had delivered it.
  pub const DELIVERY_COUNT: &str = "x-delivery-count";
}

/// A message's identity, chosen by the producer and kept by every broker and by dead-lettering,
/// so a consumer can deduplicate redeliveries on it (the inbox). It is not the broker's own
/// position (a Redis Streams entry id, a Kafka offset).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(String);

impl MessageId {
  /// The id `id` (an outbox row id, an idempotency key).
  pub fn new(id: impl Into<String>) -> Self {
    Self(id.into())
  }

  /// A fresh id: 32 hex digits, the wall-clock nanoseconds then 64 random bits, so ids sort
  /// roughly by creation time and do not collide across processes.
  pub fn generate() -> Self {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let mut random = RandomState::new().build_hasher();
    random.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    Self(format!("{nanos:016x}{:016x}", random.finish()))
  }

  /// The id.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for MessageId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl From<String> for MessageId {
  fn from(id: String) -> Self {
    Self(id)
  }
}

impl From<&str> for MessageId {
  fn from(id: &str) -> Self {
    Self(id.to_owned())
  }
}

/// A message: an id, an optional key (the partition or ordering key on brokers that have one),
/// text headers and an opaque payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
  id: MessageId,
  key: Option<String>,
  headers: BTreeMap<String, String>,
  payload: Vec<u8>,
}

impl Message {
  /// A message carrying `payload`, with a [generated](MessageId::generate) id.
  pub fn new(payload: impl Into<Vec<u8>>) -> Self {
    Self {
      id: MessageId::generate(),
      key: None,
      headers: BTreeMap::new(),
      payload: payload.into(),
    }
  }

  /// `value` encoded with its schema as JSON, with [`headers::CONTENT_TYPE`] set to
  /// [`headers::JSON`]; read it back with [`decode`](Self::decode).
  pub fn encode<T>(value: T) -> Self
  where
    T: HasSchema<A = T, I = Unknown, E = ()>,
  {
    let json = serde_json_from_unknown(&T::schema().encode(value));
    Self::new(json.to_string()).with_header(headers::CONTENT_TYPE, headers::JSON)
  }

  /// With the id `id` instead of the generated one.
  #[must_use]
  pub fn with_id(mut self, id: impl Into<MessageId>) -> Self {
    self.id = id.into();
    self
  }

  /// With the key `key`.
  #[must_use]
  pub fn with_key(mut self, key: impl Into<String>) -> Self {
    self.key = Some(key.into());
    self
  }

  /// With the header `name` set to `value` (replacing an earlier value).
  #[must_use]
  pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self.headers.insert(name.into(), value.into());
    self
  }

  /// The id.
  pub fn id(&self) -> &MessageId {
    &self.id
  }

  /// The key, if any.
  pub fn key(&self) -> Option<&str> {
    self.key.as_deref()
  }

  /// Every header, by name.
  pub fn headers(&self) -> &BTreeMap<String, String> {
    &self.headers
  }

  /// The header `name`, if set.
  pub fn header(&self, name: &str) -> Option<&str> {
    self.headers.get(name).map(String::as_str)
  }

  /// The raw payload.
  pub fn payload(&self) -> &[u8] {
    &self.payload
  }

  /// The payload, by value.
  pub fn into_payload(self) -> Vec<u8> {
    self.payload
  }

  /// The payload as UTF-8 text; [`BrokerError::Decode`] when it is not.
  pub fn text(&self) -> Result<&str, BrokerError> {
    std::str::from_utf8(&self.payload).map_err(|e| BrokerError::Decode(e.to_string()))
  }

  /// The payload decoded with `T`'s schema (what [`encode`](Self::encode) wrote);
  /// [`BrokerError::Decode`] naming every field error when it is not a JSON `T`.
  pub fn decode<T>(&self) -> Result<T, BrokerError>
  where
    T: HasSchema<A = T, I = Unknown, E = ()>,
  {
    let json: serde_json::Value =
      serde_json::from_slice(&self.payload).map_err(|e| BrokerError::Decode(e.to_string()))?;
    T::schema()
      .decode_unknown_all(&unknown_from_serde_json(json))
      .map_err(|errors| BrokerError::Decode(describe(&errors)))
  }

  /// This message as a broker moves it to a dead-letter topic: the same id, key, headers and
  /// payload, plus [`headers::DEAD_LETTER_TOPIC`], [`headers::DEAD_LETTER_GROUP`],
  /// [`headers::DEAD_LETTER_REASON`] and [`headers::DELIVERY_COUNT`].
  #[must_use]
  pub fn dead_lettered(
    self,
    from: &Topic,
    group: &ConsumerGroup,
    deliveries: u32,
    reason: DeadLetterReason,
  ) -> Self {
    self
      .with_header(headers::DEAD_LETTER_TOPIC, from.as_str())
      .with_header(headers::DEAD_LETTER_GROUP, group.as_str())
      .with_header(headers::DEAD_LETTER_REASON, reason.as_str())
      .with_header(headers::DELIVERY_COUNT, deliveries.to_string())
  }
}

/// Every issue of `errors` on one line: `path: message; …`.
fn describe(errors: &ParseErrors) -> String {
  errors
    .issues
    .iter()
    .map(|e| {
      if e.path.is_empty() {
        e.message.clone()
      } else {
        format!("{}: {}", e.path, e.message)
      }
    })
    .collect::<Vec<_>>()
    .join("; ")
}

/// A validated consumer group name, with the rules of a [`Topic`] name. Every group of a topic
/// receives every message published to it; the subscriptions of one group share them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConsumerGroup(String);

impl ConsumerGroup {
  /// Validates `name` as a group (the [`Topic`] rules, so the errors are [`TopicError`]s).
  pub fn new(name: impl Into<String>) -> Result<Self, TopicError> {
    let name = name.into();
    validate_name(&name)?;
    Ok(Self(name))
  }

  /// The group name.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl AsRef<str> for ConsumerGroup {
  fn as_ref(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for ConsumerGroup {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Where a consumer group that does not exist yet starts reading its topic. A group that exists
/// carries on from where it is, whatever a later subscription asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StartFrom {
  /// From the oldest message the broker still holds (Redis Streams `XGROUP CREATE … 0`).
  #[default]
  Earliest,
  /// From the next message published (Redis Streams `XGROUP CREATE … $`).
  Latest,
}

/// How a subscription consumes: flow control, retries and the dead-letter topic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscribeOptions {
  /// At most this many deliveries of the subscription are unsettled (handed out, not yet acked
  /// or nacked) at a time; the stream waits while it holds that many. Default
  /// [`DEFAULT_PREFETCH`](Self::DEFAULT_PREFETCH).
  pub prefetch: usize,
  /// How many handlers [`consume`] runs at once (at most `prefetch`). Default 1.
  pub concurrency: usize,
  /// A delivery nacked with [`Nack::Requeue`](crate::Nack::Requeue) (or dropped unsettled) on
  /// its `max_deliveries`-th delivery is dead-lettered instead of redelivered. Default
  /// [`DEFAULT_MAX_DELIVERIES`](Self::DEFAULT_MAX_DELIVERIES).
  pub max_deliveries: u32,
  /// The dead-letter topic (default the topic's [`Topic::dead_letter`], `<topic>.dlq`).
  pub dead_letter: Option<Topic>,
  /// Where a new group starts (default [`StartFrom::Earliest`]).
  pub start: StartFrom,
}

impl Default for SubscribeOptions {
  fn default() -> Self {
    Self {
      prefetch: Self::DEFAULT_PREFETCH,
      concurrency: 1,
      max_deliveries: Self::DEFAULT_MAX_DELIVERIES,
      dead_letter: None,
      start: StartFrom::Earliest,
    }
  }
}

impl SubscribeOptions {
  /// The default [`prefetch`](Self::prefetch).
  pub const DEFAULT_PREFETCH: usize = 16;
  /// The default [`max_deliveries`](Self::max_deliveries).
  pub const DEFAULT_MAX_DELIVERIES: u32 = 5;

  /// With [`prefetch`](Self::prefetch) `n`.
  #[must_use]
  pub fn prefetch(mut self, n: usize) -> Self {
    self.prefetch = n;
    self
  }

  /// With [`concurrency`](Self::concurrency) `n`.
  #[must_use]
  pub fn concurrency(mut self, n: usize) -> Self {
    self.concurrency = n;
    self
  }

  /// With [`max_deliveries`](Self::max_deliveries) `n`.
  #[must_use]
  pub fn max_deliveries(mut self, n: u32) -> Self {
    self.max_deliveries = n;
    self
  }

  /// Dead-letter to `topic`.
  #[must_use]
  pub fn dead_letter(mut self, topic: Topic) -> Self {
    self.dead_letter = Some(topic);
    self
  }

  /// A new group starts at `start`.
  #[must_use]
  pub fn start(mut self, start: StartFrom) -> Self {
    self.start = start;
    self
  }

  /// Checks the options of a subscription to `topic` and resolves its dead-letter topic.
  /// [`BrokerError::InvalidOptions`] for a zero `prefetch`, `concurrency` or `max_deliveries`,
  /// a `concurrency` above `prefetch`, a dead-letter topic equal to `topic`, or a default
  /// dead-letter name over [`Topic::MAX_LEN`].
  pub fn validate(&self, topic: &Topic) -> Result<Topic, BrokerError> {
    let invalid = |m: String| Err(BrokerError::InvalidOptions(m));
    if self.prefetch == 0 {
      return invalid("prefetch must be at least 1".into());
    }
    if self.concurrency == 0 {
      return invalid("concurrency must be at least 1".into());
    }
    if self.concurrency > self.prefetch {
      return invalid(format!(
        "concurrency {} exceeds prefetch {}: the extra handlers would never run",
        self.concurrency, self.prefetch
      ));
    }
    if self.max_deliveries == 0 {
      return invalid("max_deliveries must be at least 1".into());
    }
    let dead_letter = match &self.dead_letter {
      Some(t) => t.clone(),
      None => topic
        .dead_letter()
        .map_err(|e| BrokerError::InvalidOptions(format!("dead-letter topic of `{topic}`: {e}")))?,
    };
    if dead_letter == *topic {
      return invalid(format!("`{topic}` cannot be its own dead-letter topic"));
    }
    Ok(dead_letter)
  }
}

/// A broker operation's failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BrokerError {
  /// The broker is unreachable or the connection was lost (retry, or subscribe again).
  Connection(String),
  /// The broker was shut down.
  Closed,
  /// The [`SubscribeOptions`] are invalid.
  InvalidOptions(String),
  /// A payload is not what the consumer expects.
  Decode(String),
  /// The broker rejected the operation.
  Backend(String),
}

impl fmt::Display for BrokerError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      BrokerError::Connection(m) => write!(f, "broker connection: {m}"),
      BrokerError::Closed => f.write_str("the broker is closed"),
      BrokerError::InvalidOptions(m) => write!(f, "invalid subscribe options: {m}"),
      BrokerError::Decode(m) => write!(f, "message payload: {m}"),
      BrokerError::Backend(m) => write!(f, "broker: {m}"),
    }
  }
}

impl std::error::Error for BrokerError {}

/// A message broker: publish to a topic, and subscribe a consumer group to it.
///
/// Delivery is at least once: a delivery that is nacked, dropped unsettled, or whose ack is lost
/// comes again (with a higher [`attempt`](Delivery::attempt)), so handlers must be idempotent.
/// Object safe (`Arc<dyn MessageBroker>`); the typed and looping helpers are in
/// [`MessageBrokerExt`].
pub trait MessageBroker: Send + Sync + 'static {
  /// Appends `message` to `topic`, for every consumer group of the topic.
  fn publish(&self, topic: &Topic, message: Message) -> Effect<(), BrokerError>;

  /// Joins `group` on `topic` (creating the group at [`SubscribeOptions::start`] when it does
  /// not exist). The effect completes once the group exists, so every message published after
  /// it is delivered to the group; the stream then yields the group's messages to this
  /// subscription, at most [`SubscribeOptions::prefetch`] unsettled at a time, waiting while
  /// there are none. It fails with [`BrokerError::InvalidOptions`] for options that do not
  /// [`validate`](SubscribeOptions::validate); the stream ends when the broker is closed.
  fn subscribe(
    &self,
    topic: &Topic,
    group: &ConsumerGroup,
    options: SubscribeOptions,
  ) -> Effect<Stream<Delivery, BrokerError>, BrokerError>;
}

/// Schema-typed publishing and the consumer loop, on any [`MessageBroker`].
pub trait MessageBrokerExt: MessageBroker {
  /// Publishes `value` [encoded](Message::encode) with its schema.
  fn publish_encoded<T>(&self, topic: &Topic, value: T) -> Effect<(), BrokerError>
  where
    T: HasSchema<A = T, I = Unknown, E = ()>,
  {
    self.publish(topic, Message::encode(value))
  }

  /// Subscribes and runs [`consume`] over the deliveries with
  /// [`SubscribeOptions::concurrency`] handlers at once, until the stream ends or fails.
  fn consume<F>(
    &self,
    topic: &Topic,
    group: &ConsumerGroup,
    options: SubscribeOptions,
    handler: F,
  ) -> Effect<(), BrokerError>
  where
    F: Fn(Delivery) -> Effect<(), BrokerError> + Send + Sync + 'static,
  {
    let concurrency = options.concurrency;
    self
      .subscribe(topic, group, options)
      .flat_map(move |deliveries| consume(deliveries, concurrency, handler))
  }
}

impl<B: MessageBroker + ?Sized> MessageBrokerExt for B {}

/// Runs `handler` over every delivery, at most `concurrency` (at least 1) at once, until the
/// stream ends; the handler settles each delivery (see [`ack_on_success`](crate::ack_on_success)
/// for the common policy). The first failure of the stream or of a handler ends the loop with
/// it, and the deliveries still unsettled then are redelivered.
///
/// `handler` is called for a whole pulled chunk before the chunk's effects run (up to
/// `concurrency` at once, the next chunk pulled when they are all done), so keep its work inside
/// the effect it returns.
pub fn consume<F>(
  deliveries: Stream<Delivery, BrokerError>,
  concurrency: usize,
  handler: F,
) -> Effect<(), BrokerError>
where
  F: Fn(Delivery) -> Effect<(), BrokerError> + Send + Sync + 'static,
{
  deliveries.map_effect_par(concurrency, handler).run_drain()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn topic(name: &str) -> Topic {
    Topic::new(name).unwrap()
  }

  #[test]
  fn message_builder_and_accessors() {
    let m = Message::new("hi")
      .with_id("m-1")
      .with_key("acct-7")
      .with_header("trace", "abc");
    assert_eq!(m.id().as_str(), "m-1");
    assert_eq!(m.key(), Some("acct-7"));
    assert_eq!(m.header("trace"), Some("abc"));
    assert_eq!(m.header("missing"), None);
    assert_eq!(m.headers().len(), 1);
    assert_eq!(m.text(), Ok("hi"));
    assert_eq!(m.clone().into_payload(), b"hi".to_vec());
    let bad = Message::new(vec![0xff]);
    assert!(matches!(bad.text(), Err(BrokerError::Decode(_))));
    assert_eq!(bad.key(), None);
  }

  #[test]
  fn generated_ids_are_distinct_and_hex() {
    let ids: std::collections::BTreeSet<MessageId> =
      (0..100).map(|_| MessageId::generate()).collect();
    assert_eq!(ids.len(), 100);
    let id = MessageId::generate();
    assert_eq!(id.as_str().len(), 32);
    assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(MessageId::from(String::from("x")).to_string(), "x");
    assert_eq!(MessageId::new("y"), MessageId::from("y"));
  }

  #[test]
  fn dead_lettered_keeps_the_message_and_adds_headers() {
    let m = Message::new("p").with_id("m-2");
    let group = ConsumerGroup::new("billing").unwrap();
    let dead =
      m.clone()
        .dead_lettered(&topic("orders"), &group, 3, DeadLetterReason::MaxDeliveries);
    assert_eq!((dead.id(), dead.payload()), (m.id(), m.payload()));
    assert_eq!(dead.header(headers::DEAD_LETTER_TOPIC), Some("orders"));
    assert_eq!(dead.header(headers::DEAD_LETTER_GROUP), Some("billing"));
    assert_eq!(
      dead.header(headers::DEAD_LETTER_REASON),
      Some("max-deliveries")
    );
    assert_eq!(dead.header(headers::DELIVERY_COUNT), Some("3"));
  }

  #[test]
  fn consumer_group_names_follow_topic_rules() {
    let g = ConsumerGroup::new("billing-v2").unwrap();
    assert_eq!(
      (g.as_str(), g.as_ref(), g.to_string()),
      ("billing-v2", "billing-v2", "billing-v2".into())
    );
    assert_eq!(ConsumerGroup::new(""), Err(TopicError::Empty));
    assert_eq!(
      ConsumerGroup::new("a b"),
      Err(TopicError::InvalidChar { ch: ' ', index: 1 })
    );
  }

  #[test]
  fn options_defaults_and_builders() {
    let d = SubscribeOptions::default();
    assert_eq!(
      (d.prefetch, d.concurrency, d.max_deliveries, d.start),
      (16, 1, 5, StartFrom::Earliest)
    );
    assert_eq!(d.validate(&topic("orders")), Ok(topic("orders.dlq")));
    let o = SubscribeOptions::default()
      .prefetch(4)
      .concurrency(2)
      .max_deliveries(9)
      .dead_letter(topic("poison"))
      .start(StartFrom::Latest);
    assert_eq!(
      (o.prefetch, o.concurrency, o.max_deliveries, o.start),
      (4, 2, 9, StartFrom::Latest)
    );
    assert_eq!(o.validate(&topic("orders")), Ok(topic("poison")));
  }

  #[test]
  fn invalid_options_are_rejected() {
    let t = topic("orders");
    let invalid =
      |o: SubscribeOptions| matches!(o.validate(&t), Err(BrokerError::InvalidOptions(_)));
    assert!(invalid(SubscribeOptions::default().prefetch(0)));
    assert!(invalid(SubscribeOptions::default().concurrency(0)));
    assert!(invalid(
      SubscribeOptions::default().prefetch(2).concurrency(3)
    ));
    assert!(invalid(SubscribeOptions::default().max_deliveries(0)));
    assert!(invalid(
      SubscribeOptions::default().dead_letter(topic("orders"))
    ));
    let long = topic(&"a".repeat(Topic::MAX_LEN));
    assert!(matches!(
      SubscribeOptions::default().validate(&long),
      Err(BrokerError::InvalidOptions(m)) if m.contains("dead-letter")
    ));
  }

  #[test]
  fn errors_display() {
    let texts: Vec<String> = [
      BrokerError::Connection("down".into()),
      BrokerError::Closed,
      BrokerError::InvalidOptions("x".into()),
      BrokerError::Decode("y".into()),
      BrokerError::Backend("z".into()),
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
      texts,
      [
        "broker connection: down",
        "the broker is closed",
        "invalid subscribe options: x",
        "message payload: y",
        "broker: z"
      ]
    );
  }
}
