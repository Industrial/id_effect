//! [`StoreError`]: how a [`TodoRepository`](super::TodoRepository) call can fail.

use std::fmt;

use id_effect::EffectData;

/// A store failure. Domain outcomes (a missing todo, a taken title) are values of the
/// repository's results, not store failures, so a circuit breaker over the store only counts
/// these.
#[derive(Clone, Debug, EffectData)]
pub enum StoreError {
  /// The store cannot be reached right now (no statement ran): retrying may help.
  Unavailable(String),
  /// Any other failure; the detail stays on the server.
  Internal(String),
}

impl StoreError {
  /// Whether retrying may help: only [`Self::Unavailable`].
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Unavailable(_))
  }
}

impl fmt::Display for StoreError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Unavailable(why) => write!(f, "the store is unavailable: {why}"),
      Self::Internal(detail) => write!(f, "internal: {detail}"),
    }
  }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_unavailable_is_transient() {
    assert!(StoreError::Unavailable("down".into()).is_transient());
    assert!(!StoreError::Internal("bug".into()).is_transient());
    assert_eq!(
      StoreError::Unavailable("down".into()).to_string(),
      "the store is unavailable: down"
    );
  }
}
