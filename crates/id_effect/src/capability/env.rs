//! [`Env`] — order-independent capability container.

use super::error::{CapabilityError, CapabilityPlannerError};
use super::graph::CapabilityGraph;
use super::key::CapabilityKey;
use super::provider::ProviderBox;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Runtime environment holding capability services (order-independent).
#[derive(Clone, Default)]
pub struct Env {
  cells: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl fmt::Debug for Env {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Env")
      .field("len", &self.cells.len())
      .finish()
  }
}

impl PartialEq for Env {
  fn eq(&self, other: &Self) -> bool {
    self.cells.len() == other.cells.len() && self.cells.keys().all(|k| other.cells.contains_key(k))
  }
}

impl Eq for Env {}

impl Env {
  /// Empty environment.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// Insert or replace a capability value.
  pub fn insert<K>(&mut self, value: K::Value) -> &mut Self
  where
    K: CapabilityKey,
  {
    let id = K::id().type_id();
    self.cells.insert(id, Arc::new(value));
    self
  }

  /// Borrow capability `K`.
  ///
  /// # Panics
  ///
  /// When `K` is absent. The message names the capability and the caller's `file:line`
  /// (`#[track_caller]`); use [`Self::try_get`] to handle absence.
  #[inline]
  #[track_caller]
  pub fn get<K>(&self) -> &K::Value
  where
    K: CapabilityKey,
  {
    match self.try_get::<K>() {
      Ok(value) => value,
      Err(err) => missing_capability_panic(&err),
    }
  }

  /// Fallible lookup; the error records the caller's location ([`CapabilityError::MissingAt`]).
  #[track_caller]
  pub fn try_get<K>(&self) -> Result<&K::Value, CapabilityError>
  where
    K: CapabilityKey,
  {
    let location = std::panic::Location::caller();
    self.lookup::<K>().ok_or(CapabilityError::MissingAt {
      cap: K::id(),
      location,
    })
  }

  /// Lookup without call-site context (environment verification).
  pub(crate) fn lookup<K>(&self) -> Option<&K::Value>
  where
    K: CapabilityKey,
  {
    self
      .cells
      .get(&K::id().type_id())?
      .downcast_ref::<K::Value>()
  }

  /// Whether capability `K` is present.
  pub fn has<K>(&self) -> bool
  where
    K: CapabilityKey,
  {
    self.cells.contains_key(&K::id().type_id())
  }

  /// Number of registered capabilities.
  #[inline]
  pub fn len(&self) -> usize {
    self.cells.len()
  }

  /// True when no capabilities are registered.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.cells.is_empty()
  }

  /// Insert a generic cell keyed by `TypeId::of::<T>()` (non-object-safe traits).
  pub fn insert_any<T: Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
    self.cells.insert(TypeId::of::<T>(), value);
    self
  }

  /// Borrow a generic cell inserted via [`Self::insert_any`].
  #[track_caller]
  pub fn get_any<T: Send + Sync + 'static>(&self) -> Result<Arc<T>, CapabilityError> {
    let missing = CapabilityError::MissingAt {
      cap: super::id::CapabilityId::of::<T>(),
      location: std::panic::Location::caller(),
    };
    let cell = self.cells.get(&TypeId::of::<T>()).ok_or(missing.clone())?;
    cell.clone().downcast::<T>().map_err(|_| missing)
  }

  /// Build additional providers on a clone of this environment (scoped child).
  pub fn scoped<I>(&self, providers: I) -> Result<Env, CapabilityPlannerError>
  where
    I: IntoIterator<Item = ProviderBox>,
  {
    let mut graph = CapabilityGraph::new();
    for p in providers {
      graph = graph.add(p.0);
    }
    graph.build_from(self.clone())
  }
}

/// Effect environment type alias (runtime container).
pub type Caps = Env;

/// Panic for a capability lookup that must succeed ([`Env::get`], [`super::Needs::need`]).
#[cold]
#[track_caller]
pub(crate) fn missing_capability_panic(err: &CapabilityError) -> ! {
  panic!(
    "{err}\n  hint: register a provider with `provide!(…)` in `run_with` / `build_env`, \
     and list the service in the effect's `caps!(…)` so `run_with` verifies it up front"
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Cap;
  use crate::provide;
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  struct Counter(pub u32);
  #[derive(Clone, Copy)]
  #[allow(dead_code)]
  struct Name;

  #[derive(::id_effect::ProviderSpecDerive)]
  #[provides(Counter, with = || Counter(1))]
  struct CounterLive;

  #[test]
  fn env_insert_get_and_has() {
    let mut env = Env::new();
    assert!(env.is_empty());
    env.insert::<Cap<Counter>>(Counter(42));
    assert_eq!(env.len(), 1);
    assert!(env.has::<Cap<Counter>>());
    assert_eq!(env.get::<Cap<Counter>>().0, 42);
  }

  #[test]
  fn env_try_get_missing() {
    let env = Env::new();
    let err = env.try_get::<Cap<Counter>>().unwrap_err();
    let line = line!() - 1;
    assert!(matches!(err, CapabilityError::MissingAt { .. }));
    let loc = err.location().expect("call site");
    assert_eq!((loc.file(), loc.line()), (file!(), line));
    assert!(err.to_string().contains("Counter"), "{err}");
  }

  #[test]
  fn env_get_missing_panics_with_service_name_and_call_site() {
    let env = Env::new();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let _ = env.get::<Cap<Counter>>();
    }))
    .expect_err("missing capability must panic");
    let line = line!() - 3;
    let msg = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("missing capability"), "{msg}");
    assert!(msg.contains("Counter"), "{msg}");
    assert!(msg.contains(&format!("{}:{line}", file!())), "{msg}");
    assert!(msg.contains("provide!"), "{msg}");
  }

  #[test]
  fn env_get_and_try_get_after_insert() {
    let mut env = Env::new();
    env.insert::<Cap<Counter>>(Counter(7));
    assert_eq!(env.get::<Cap<Counter>>().0, 7);
    assert_eq!(env.try_get::<Cap<Counter>>().unwrap().0, 7);
  }

  #[test]
  fn env_debug_and_eq() {
    let mut a = Env::new();
    a.insert::<Cap<Counter>>(Counter(1));
    let mut b = Env::new();
    b.insert::<Cap<Counter>>(Counter(2));
    assert_eq!(format!("{a:?}"), "Env { len: 1 }");
    assert_eq!(a, b);
  }

  #[test]
  fn env_insert_any_roundtrip() {
    let mut env = Env::new();
    env.insert_any(Arc::new(99_i64));
    let v = env.get_any::<i64>().unwrap();
    assert_eq!(*v, 99);
  }

  #[test]
  fn env_scoped_builds_child() {
    let parent = Env::new();
    let child = parent.scoped([provide!(CounterLive)]).unwrap();
    assert!(child.has::<Cap<Counter>>());
  }

  #[test]
  fn env_get_any_missing() {
    let env = Env::new();
    assert!(env.get_any::<i64>().is_err());
  }
}
