//! Stable capability identifiers.

use std::any::TypeId;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Type-level identity for a capability (usually the generated key type).
///
/// Equality and hashing use the type identity and variant only; the human-readable
/// [`name`](Self::name) is carried for diagnostics.
#[derive(Clone, Copy)]
pub struct CapabilityId {
  type_id: TypeId,
  variant: Option<&'static str>,
  name: &'static str,
}

impl PartialEq for CapabilityId {
  fn eq(&self, other: &Self) -> bool {
    self.type_id == other.type_id && self.variant == other.variant
  }
}

impl Eq for CapabilityId {}

impl Hash for CapabilityId {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.type_id.hash(state);
    self.variant.hash(state);
  }
}

impl CapabilityId {
  /// Identity of type `T` (typically a generated `*Key` struct).
  #[inline]
  pub fn of<T: 'static>() -> Self {
    Self {
      type_id: TypeId::of::<T>(),
      variant: None,
      name: std::any::type_name::<T>(),
    }
  }

  /// Attach an optional named variant label.
  #[inline]
  pub fn with_variant(self, variant: Option<&'static str>) -> Self {
    Self { variant, ..self }
  }

  /// Replace the diagnostic name (identity is unchanged).
  #[inline]
  pub fn with_name(self, name: &'static str) -> Self {
    Self { name, ..self }
  }

  /// Human-readable name for diagnostics (the service type for [`Cap<T>`](super::Cap) slots).
  #[inline]
  pub fn name(self) -> &'static str {
    self.name
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
        .field(&self.name)
        .field(&v)
        .finish(),
      None => f.debug_tuple("CapabilityId").field(&self.name).finish(),
    }
  }
}

/// `name` or `name#variant`.
impl fmt::Display for CapabilityId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.variant {
      Some(v) => write!(f, "{}#{v}", self.name),
      None => f.write_str(self.name),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn capability_id_of_and_variant() {
    let id = CapabilityId::of::<u32>();
    assert_eq!(id.type_id(), TypeId::of::<u32>());
    assert_eq!(id.variant(), None);
    let named = id.with_variant(Some("live"));
    assert_eq!(named.variant(), Some("live"));
    assert_eq!(format!("{named:?}"), format!("{:?}", named));
    assert_eq!(id.name(), "u32");
    assert_eq!(named.to_string(), "u32#live");
  }

  #[test]
  fn capability_id_identity_ignores_name() {
    let id = CapabilityId::of::<u32>();
    let renamed = id.with_name("Counter");
    assert_eq!(id, renamed);
    assert_eq!(renamed.name(), "Counter");
    assert_eq!(renamed.to_string(), "Counter");
  }
}
