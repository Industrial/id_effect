//! [`Needs`] — borrow a capability from the environment.

use super::env::Env;
use super::key::CapabilityKey;
use crate::context::{Get, Here};

/// Environment `R` provides capability `K`.
pub trait Needs<K: CapabilityKey> {
  /// Borrow the service registered for `K`.
  fn need(&self) -> &K::Value;
}

impl<K, L> Needs<K> for crate::context::Context<L>
where
  K: CapabilityKey,
  L: Get<K, Here, Target = K::Value>,
{
  #[inline]
  fn need(&self) -> &K::Value {
    Get::<K, Here>::get(&self.0)
  }
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
