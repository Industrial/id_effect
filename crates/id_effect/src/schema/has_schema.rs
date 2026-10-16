//! [`HasSchema`] — opt-in hook for types that expose a canonical [`crate::schema::parse::Schema`].
//!
//! Implement by hand or with `#[derive(Schema)]` (structs with named fields).
//!
//! Tests follow repository [`TESTING.md`](../../../../TESTING.md).

use crate::schema::ast::{LazyAst, SchemaAst, SchemaKind};
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

/// Deferred, named reference to `T`'s [`SchemaAst`]: a [`SchemaKind::Lazy`] node that calls
/// `T::schema().ast()` only when forced.
///
/// The name is `T`'s [`type_name`](std::any::type_name) with every character outside
/// `[A-Za-z0-9_.:-]` replaced by `_`, so JSON Schema export emits `T` once under `$defs` and
/// `$ref`s it (a valid URI fragment). Because nothing is built eagerly, recursive types
/// terminate. `#[derive(Schema)]` uses this for fields of other `HasSchema` types. Type names are
/// descriptive only and may change between compiler versions.
pub fn ast_ref<T: HasSchema + 'static>() -> SchemaAst {
  let name: String = std::any::type_name::<T>()
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
        c
      } else {
        '_'
      }
    })
    .collect();
  SchemaAst::new(SchemaKind::Lazy(LazyAst::new(Some(name), || {
    T::schema().ast().clone()
  })))
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

  struct Generic<T>(std::marker::PhantomData<T>);

  impl<T: 'static> HasSchema for Generic<T> {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<Self::A, Self::I, Self::E> {
      i64::<()>()
    }
  }

  #[test]
  fn ast_ref_is_a_named_lazy_with_a_fragment_safe_name() {
    let SchemaKind::Lazy(lazy) = ast_ref::<OnlyInt>().kind else {
      panic!("expected a lazy node");
    };
    assert!(lazy.name().unwrap().ends_with("::OnlyInt"));
    assert_eq!(lazy.force().kind, SchemaKind::Integer);

    let SchemaKind::Lazy(lazy) = ast_ref::<Generic<OnlyInt>>().kind else {
      panic!("expected a lazy node");
    };
    let name = lazy.name().unwrap();
    assert!(
      name.contains("Generic_") && name.ends_with("OnlyInt_"),
      "{name}"
    );
  }
}
