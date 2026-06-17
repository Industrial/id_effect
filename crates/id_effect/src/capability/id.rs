//! Stable capability identifiers.

use std::any::TypeId;
use std::fmt;

/// Type-level identity for a capability (usually the generated key type).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapabilityId(TypeId);

impl CapabilityId {
  /// Identity of type `T` (typically a generated `*Key` struct).
  #[inline]
  pub fn of<T: 'static>() -> Self {
    Self(TypeId::of::<T>())
  }

  /// Underlying [`TypeId`].
  #[inline]
  pub fn type_id(self) -> TypeId {
    self.0
  }
}

impl fmt::Debug for CapabilityId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("CapabilityId").field(&self.0).finish()
  }
}
