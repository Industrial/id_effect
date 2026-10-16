//! [`TodoId`]: the identity of a [`Todo`](super::Todo).

use std::fmt;

use id_effect::EffectData;

/// The store-assigned identity of a todo.
#[derive(Clone, Copy, Debug, PartialOrd, Ord, EffectData)]
pub struct TodoId(i64);

impl TodoId {
  /// The id `raw`, as assigned by the store.
  pub const fn new(raw: i64) -> Self {
    Self(raw)
  }

  /// The raw id.
  pub const fn get(self) -> i64 {
    self.0
  }
}

impl fmt::Display for TodoId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn round_trips_and_displays() {
    let id = TodoId::new(7);
    assert_eq!(id.get(), 7);
    assert_eq!(id.to_string(), "7");
    assert_eq!(id, TodoId::new(7));
    assert!(TodoId::new(1) < id);
  }
}
