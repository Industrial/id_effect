//! [`Needs`] — borrow a capability from the environment.

use super::env::Env;
use super::key::CapabilityKey;
use std::ops::Deref;

/// Environment `R` provides capability `K`.
pub trait Needs<K: CapabilityKey> {
  /// Borrow the service registered for `K`.
  fn need(&self) -> &K::Value;
}

impl<K> Needs<K> for Env
where
  K: CapabilityKey,
{
  #[inline]
  fn need(&self) -> &K::Value {
    self.get::<K>()
  }
}

impl<K, T> Needs<K> for T
where
  K: CapabilityKey,
  T: Deref<Target = Env>,
{
  #[inline]
  fn need(&self) -> &K::Value {
    self.deref().get::<K>()
  }
}
