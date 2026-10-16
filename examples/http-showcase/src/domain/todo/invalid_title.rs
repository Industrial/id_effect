//! [`InvalidTitle`]: why a raw string is not a [`Title`](super::Title).

use std::fmt;

use id_effect::EffectData;

/// A rejected title.
#[derive(Clone, Debug, EffectData)]
pub enum InvalidTitle {
  /// Empty once trimmed.
  Empty,
  /// Longer than `max` characters once trimmed.
  TooLong {
    /// The limit ([`Title::MAX_CHARS`](super::Title::MAX_CHARS)).
    max: usize,
  },
}

impl fmt::Display for InvalidTitle {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Empty => f.write_str("the title must not be empty"),
      Self::TooLong { max } => write!(f, "the title must be at most {max} characters"),
    }
  }
}

impl std::error::Error for InvalidTitle {}
