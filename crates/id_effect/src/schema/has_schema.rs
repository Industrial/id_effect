//! [`HasSchema`] — opt-in hook for types that expose a canonical [`crate::schema::parse::Schema`].
//!
//! A future `#[derive(Schema)]` could implement this automatically; for now implement by hand.
//!
//! Tests follow repository [`TESTING.md`](../../../../TESTING.md).

use crate::schema::data::EffectData;
use crate::schema::parse::Schema;

/// A type that knows its bidirectional [`Schema`].
pub trait HasSchema {
  /// Semantic (decoded) type.
  type A: 'static;
  /// Wire / encoded type.
  type I: 'static;
  /// [`EffectData`] tag for the schema.
  type E: EffectData + 'static;

  /// Canonical schema instance (typically a `'static` singleton or cheap [`Clone`]).
  fn schema() -> Schema<Self::A, Self::I, Self::E>
  where
    Self: Sized;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::schema::parse::{Schema, i64};

  struct OnlyInt;

  impl HasSchema for OnlyInt {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<Self::A, Self::I, Self::E> {
      i64::<()>()
    }
  }

  #[test]
  fn manual_impl_returns_schema() {
    let s = OnlyInt::schema();
    assert_eq!(s.decode(3_i64).unwrap(), 3);
  }
}
