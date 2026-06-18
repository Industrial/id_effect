//! Stable capability identifiers.

use std::any::TypeId;
use std::fmt;

/// Type-level identity for a capability (usually the generated key type).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapabilityId {
  type_id: TypeId,
  variant: Option<&'static str>,
}

impl CapabilityId {
  /// Identity of type `T` (typically a generated `*Key` struct).
  #[inline]
  pub fn of<T: 'static>() -> Self {
    Self {
      type_id: TypeId::of::<T>(),
      variant: None,
    }
  }

  /// Attach an optional named variant label.
  #[inline]
  pub fn with_variant(self, variant: Option<&'static str>) -> Self {
    Self {
      type_id: self.type_id,
      variant,
    }
  }

  /// Underlying [`TypeId`].
  #[inline]
  pub fn type_id(self) -> TypeId {
    self.type_id
  }

  /// Optional named variant label.
  #[inline]
  pub fn variant(self) -> Option<&'static str> {
    self.variant
  }
}

impl fmt::Debug for CapabilityId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.variant {
      Some(v) => f
        .debug_tuple("CapabilityId")
        .field(&self.type_id)
        .field(&v)
        .finish(),
      None => f.debug_tuple("CapabilityId").field(&self.type_id).finish(),
    }
  }
}
