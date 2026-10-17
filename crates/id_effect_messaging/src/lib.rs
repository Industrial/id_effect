//! Messaging for id_effect services: a broker-agnostic `MessageBroker`, an in-memory broker, and
//! the transactional outbox, inbox and relay on `id_effect_sql`.
//!
//! [`Topic`] is the validated name messages are published to: 1 to [`Topic::MAX_LEN`] ASCII
//! letters, digits, `.`, `_` or `-`, a name every supported broker accepts.
//!
//! ```
//! use id_effect_messaging::{Topic, TopicError};
//!
//! let topic = Topic::new("ledger.entry-posted").unwrap();
//! assert_eq!(topic.to_string(), "ledger.entry-posted");
//! assert_eq!(Topic::new("a b"), Err(TopicError::InvalidChar { ch: ' ', index: 1 }));
//! ```
//!
//! # Brokers
//!
//! A [`MessageBroker`] publishes [`Message`]s to a topic and hands them to **consumer groups**:
//! every group sees every message of the topic, and the consumers of one group share them. A
//! subscription is a [`Stream`](id_effect::Stream) of [`Delivery`]s; each is settled once, with
//! [`ack`](Delivery::ack) or [`nack`](Delivery::nack). A nacked (or dropped) delivery is
//! redelivered until [`SubscribeOptions::max_deliveries`], then moved to the dead-letter topic;
//! at most [`SubscribeOptions::prefetch`] deliveries of a subscription are unsettled at a time.
//! [`MemoryBroker`] implements all of it in memory, for tests and single-process development.
//!
//! ```
//! use id_effect_messaging::{
//!   ConsumerGroup, MemoryBroker, Message, MessageBroker, Nack, SubscribeOptions, Topic,
//! };
//!
//! let broker = MemoryBroker::new();
//! let orders = Topic::new("orders").unwrap();
//! let billing = ConsumerGroup::new("billing").unwrap();
//! let opts = SubscribeOptions::default().max_deliveries(2);
//! let eff = broker
//!   .publish(&orders, Message::new("order 1"))
//!   .flat_map({
//!     let (broker, orders, billing) = (broker.clone(), orders.clone(), billing.clone());
//!     move |()| broker.subscribe(&orders, &billing, opts)
//!   })
//!   .flat_map(|deliveries| deliveries.take(2).run_for_each(|d| d.nack(Nack::Requeue)));
//! assert!(eff.block_on().into_result().is_ok());
//! // Delivered twice, nacked twice: dead-lettered.
//! let dlq = broker.messages(&Topic::new("orders.dlq").unwrap());
//! assert_eq!(dlq[0].payload(), b"order 1");
//! ```
//!
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod broker;
mod delivery;
mod memory;

pub use broker::{
  BrokerError, ConsumerGroup, Message, MessageBroker, MessageBrokerExt, MessageId, StartFrom,
  SubscribeOptions, consume, headers,
};
pub use delivery::{Acker, DeadLetterReason, Delivery, Nack, ack_on_success};
pub use memory::MemoryBroker;

use std::fmt;

/// A validated topic name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Topic(String);

impl Topic {
  /// Longest topic name, in bytes (Kafka's limit, the strictest of the brokers).
  pub const MAX_LEN: usize = 249;

  /// The suffix of a topic's default dead-letter topic.
  pub const DEAD_LETTER_SUFFIX: &'static str = ".dlq";

  /// Validates `name` as a topic.
  pub fn new(name: impl Into<String>) -> Result<Self, TopicError> {
    let name = name.into();
    validate_name(&name)?;
    Ok(Topic(name))
  }

  /// The topic name.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// This topic's default dead-letter topic: its name with
  /// [`DEAD_LETTER_SUFFIX`](Self::DEAD_LETTER_SUFFIX) (`orders` → `orders.dlq`);
  /// [`TopicError::TooLong`] for a name within 4 bytes of the limit.
  pub fn dead_letter(&self) -> Result<Topic, TopicError> {
    Topic::new(format!("{}{}", self.0, Self::DEAD_LETTER_SUFFIX))
  }
}

/// The rules of a [`Topic`] (and a [`ConsumerGroup`]) name.
pub(crate) fn validate_name(name: &str) -> Result<(), TopicError> {
  if name.is_empty() {
    return Err(TopicError::Empty);
  }
  if let Some((index, ch)) = name
    .char_indices()
    .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')))
  {
    return Err(TopicError::InvalidChar { ch, index });
  }
  if name.len() > Topic::MAX_LEN {
    return Err(TopicError::TooLong { len: name.len() });
  }
  Ok(())
}

impl AsRef<str> for Topic {
  fn as_ref(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for Topic {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Why a name is not a valid [`Topic`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicError {
  /// The name is empty.
  Empty,
  /// The name is longer than [`Topic::MAX_LEN`] bytes.
  TooLong {
    /// Length of the name, in bytes.
    len: usize,
  },
  /// The name contains a character other than an ASCII letter, digit, `.`, `_` or `-`.
  InvalidChar {
    /// The first offending character.
    ch: char,
    /// Its byte offset in the name.
    index: usize,
  },
}

impl fmt::Display for TopicError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TopicError::Empty => f.write_str("topic name is empty"),
      TopicError::TooLong { len } => write!(
        f,
        "topic name is {len} bytes, longer than {}",
        Topic::MAX_LEN
      ),
      TopicError::InvalidChar { ch, index } => {
        write!(f, "topic name has {ch:?} at byte {index}")
      }
    }
  }
}

impl std::error::Error for TopicError {}
