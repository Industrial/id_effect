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
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

/// A validated topic name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Topic(String);

impl Topic {
  /// Longest topic name, in bytes (Kafka's limit, the strictest of the brokers).
  pub const MAX_LEN: usize = 249;

  /// Validates `name` as a topic.
  pub fn new(name: impl Into<String>) -> Result<Self, TopicError> {
    let name = name.into();
    if name.is_empty() {
      return Err(TopicError::Empty);
    }
    if let Some((index, ch)) = name
      .char_indices()
      .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')))
    {
      return Err(TopicError::InvalidChar { ch, index });
    }
    if name.len() > Self::MAX_LEN {
      return Err(TopicError::TooLong { len: name.len() });
    }
    Ok(Topic(name))
  }

  /// The topic name.
  pub fn as_str(&self) -> &str {
    &self.0
  }
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
