//! `DynEnv`: a runtime-checked environment, with `MissingCapability` and `try_need`. Filled by
//! `k-typed-r`.
//!
//! The typed environment (`R`, an HList) is the default: a missing capability is a compile
//! error. `DynEnv` is the explicit opt-in for plugin-style wiring whose capabilities are not
//! known at compile time (ADR 0003 Amendment 1, ADR 0011). It is a `TypeId`-keyed map, and every
//! lookup is fallible: [`DynEnv::try_get`] and [`try_need`] return [`MissingCapability`]. There
//! is no panicking `get`, so `need` still never panics.
//!
//! A `DynEnv` is itself a capability: an effect that looks things up dynamically requires
//! `HCons<DynEnv, HNil>` (or any `R` containing `DynEnv`).
//!
//! ```
//! use std::sync::Arc;
//! use id_effect::next::{DynEnv, MissingCapability};
//!
//! struct Plugin(&'static str);
//! struct Absent;
//!
//! let mut env = DynEnv::new();
//! env.insert(Plugin("audit"));
//! let p: Arc<Plugin> = env.try_get::<Plugin>().map_err(|e| e.to_string())?;
//! assert_eq!(p.0, "audit");
//! assert!(matches!(env.try_get::<Absent>(), Err(MissingCapability { .. })));
//! # Ok::<(), String>(())
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::next::capability::{HCons, HNil, need};
use crate::next::kernel::{Effect, fail, succeed};

type Slot = (&'static str, Arc<dyn Any + Send + Sync>);

/// A runtime-checked environment: capabilities keyed by type, each shared as an `Arc`. Cloning
/// costs one refcount bump per capability.
#[derive(Clone, Default)]
pub struct DynEnv {
  map: HashMap<TypeId, Slot>,
}

impl fmt::Debug for DynEnv {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut names: Vec<&str> = self.map.values().map(|(name, _)| *name).collect();
    names.sort_unstable();
    f.debug_struct("DynEnv")
      .field("capabilities", &names)
      .finish()
  }
}

impl DynEnv {
  /// An empty environment.
  pub fn new() -> Self {
    Self::default()
  }

  /// Inserts `value` as capability `T`, replacing an earlier `T`.
  pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self {
    self.insert_arc(Arc::new(value))
  }

  /// Inserts an already shared capability `T`, replacing an earlier `T`.
  pub fn insert_arc<T: Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
    self
      .map
      .insert(TypeId::of::<T>(), (std::any::type_name::<T>(), value));
    self
  }

  /// Builder form of [`insert`](Self::insert).
  #[must_use]
  pub fn with<T: Send + Sync + 'static>(mut self, value: T) -> Self {
    self.insert(value);
    self
  }

  /// Removes capability `T`, returning it when it was present.
  pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<Arc<T>> {
    let (_, slot) = self.map.remove(&TypeId::of::<T>())?;
    slot.downcast::<T>().ok()
  }

  /// Capability `T`, or [`MissingCapability`] naming `T` when it was never inserted.
  pub fn try_get<T: Send + Sync + 'static>(&self) -> Result<Arc<T>, MissingCapability> {
    self
      .map
      .get(&TypeId::of::<T>())
      .and_then(|(_, slot)| Arc::clone(slot).downcast::<T>().ok())
      .ok_or_else(MissingCapability::of::<T>)
  }

  /// `true` when capability `T` is present.
  pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
    self.map.contains_key(&TypeId::of::<T>())
  }

  /// Number of capabilities.
  pub fn len(&self) -> usize {
    self.map.len()
  }

  /// `true` when no capability is present.
  pub fn is_empty(&self) -> bool {
    self.map.is_empty()
  }
}

/// A [`DynEnv`] lookup found no capability of the requested type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MissingCapability {
  /// `std::any::type_name` of the requested capability.
  pub type_name: &'static str,
}

impl MissingCapability {
  /// The error for a missing `T`.
  pub fn of<T: ?Sized>() -> Self {
    Self {
      type_name: std::any::type_name::<T>(),
    }
  }
}

impl fmt::Display for MissingCapability {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "capability `{}` is not in the dynamic environment (`DynEnv`)",
      self.type_name
    )
  }
}

impl std::error::Error for MissingCapability {}

/// Looks up capability `T` in the effect's [`DynEnv`]; fails with [`MissingCapability`] (never
/// panics) when it is absent. Widen `R` with `.narrow()` or bind it with `~` to use it in an
/// effect over a larger environment.
pub fn try_need<T: Send + Sync + 'static>() -> Effect<Arc<T>, MissingCapability, HCons<DynEnv, HNil>>
{
  need::<DynEnv>().flat_map(|env| match env.try_get::<T>() {
    Ok(t) => succeed(t),
    Err(e) => fail(e),
  })
}
