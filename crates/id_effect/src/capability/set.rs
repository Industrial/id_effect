//! Type-level capability set markers.

use super::env::Env;
use super::key::CapabilityKey;

/// Marker: effect requires capabilities listed in the `caps!` macro (runtime: [`Env`]).
pub trait CapabilitySet {
  /// Verify `env` contains all required capabilities (no-op for empty set).
  fn verify(_env: &Env) -> Result<(), super::error::CapabilityError> {
    Ok(())
  }
}

/// Empty capability set (pure effects use `()`; multi-cap uses [`Env`]).
pub struct NoCaps;

impl CapabilitySet for NoCaps {}

impl CapabilitySet for Env {}

impl CapabilitySet for () {}

/// Helper trait: `R` has capability `K`.
pub trait HasCap<K: CapabilityKey> {}

impl<K> HasCap<K> for Env where K: CapabilityKey {}
