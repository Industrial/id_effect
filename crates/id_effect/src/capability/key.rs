//! Capability slots and the [`CapabilityKey`] trait.

use std::marker::PhantomData;

use super::id::CapabilityId;

/// Type-level slot for a service `T` stored in [`super::Env`].
///
/// Use service types directly in [`caps`](crate::caps), [`require`](crate::require), and
/// `#[provides(T)]` — for example `caps!(EffectLogger)` rather than a separate key type.
#[derive(Debug, Default, PartialEq, Eq, Hash)]
pub struct Cap<T>(PhantomData<fn() -> T>);

impl<T> Copy for Cap<T> {}

impl<T> Clone for Cap<T> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<T> Cap<T> {
  /// Human-readable slot name (the value type).
  pub fn slot_name() -> &'static str {
    std::any::type_name::<T>()
  }
}

/// Associates a capability slot with the concrete service value stored in [`super::Env`].
pub trait CapabilityKey: Copy + Send + Sync + 'static {
  /// Concrete service type (cloneable handle stored in the environment).
  type Value: Clone + Send + Sync + 'static;

  /// Stable id for graph planning and diagnostics.
  fn id() -> CapabilityId {
    CapabilityId::of::<Self>()
  }

  /// Human-readable name for diagnostics (defaults to type name).
  fn name() -> &'static str {
    std::any::type_name::<Self>()
  }
}

impl<T> CapabilityKey for Cap<T>
where
  T: Clone + Send + Sync + 'static,
{
  type Value = T;
}

/// Marker implemented by all capability service traits.
pub trait Capability: Send + Sync + 'static {}
impl<T: Send + Sync + 'static> Capability for T {}

#[cfg(test)]
mod tests {
  use super::*;

  #[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
  struct Sample(u8);

  #[test]
  fn cap_slot_name_and_marker_traits() {
    assert!(Cap::<Sample>::slot_name().contains("Sample"));
    let cap: Cap<Sample> = Default::default();
    let _ = cap.clone();
    let _ = cap;
  }
}
