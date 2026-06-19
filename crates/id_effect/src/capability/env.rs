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
  #[inline]
  pub fn get<K>(&self) -> &K::Value
  where
    K: CapabilityKey,
  {
    self.try_get::<K>().expect("capability missing in Env")
  }

  /// Fallible lookup.
  pub fn try_get<K>(&self) -> Result<&K::Value, CapabilityError>
  where
    K: CapabilityKey,
  {
    let id = K::id().type_id();
    let cell = self
      .cells
      .get(&id)
      .ok_or(CapabilityError::Missing(K::id()))?;
    cell
      .downcast_ref::<K::Value>()
      .ok_or(CapabilityError::Missing(K::id()))
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
  pub fn get_any<T: Send + Sync + 'static>(&self) -> Result<Arc<T>, CapabilityError> {
    let id = TypeId::of::<T>();
    let cell = self
      .cells
      .get(&id)
      .ok_or(CapabilityError::Missing(super::id::CapabilityId::of::<T>()))?;
    cell
      .clone()
      .downcast::<T>()
      .map_err(|_| CapabilityError::Missing(super::id::CapabilityId::of::<T>()))
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::provide;

  #[::id_effect::capability(u32)]
  #[expect(dead_code)]
  struct Counter;
  #[::id_effect::capability(String)]
  #[expect(dead_code)]
  struct Name;

  #[derive(::id_effect::ProviderSpecDerive)]
  #[provides(CounterKey)]
  struct CounterLive;
  impl CounterLive {
    #[allow(clippy::new_ret_no_self)]
    fn new() -> u32 {
      1
    }
  }

  #[test]
  fn env_insert_get_and_has() {
    let mut env = Env::new();
    assert!(env.is_empty());
    env.insert::<CounterKey>(42);
    assert_eq!(env.len(), 1);
    assert!(env.has::<CounterKey>());
    assert_eq!(*env.get::<CounterKey>(), 42);
  }

  #[test]
  fn env_try_get_missing() {
    let env = Env::new();
    let err = env.try_get::<CounterKey>().unwrap_err();
    assert!(matches!(err, CapabilityError::Missing(_)));
  }

  #[test]
  fn env_get_and_try_get_after_insert() {
    let mut env = Env::new();
    env.insert::<CounterKey>(7u32);
    assert_eq!(*env.get::<CounterKey>(), 7);
    assert_eq!(*env.try_get::<CounterKey>().unwrap(), 7);
  }

  #[test]
  fn env_debug_and_eq() {
    let mut a = Env::new();
    a.insert::<CounterKey>(1);
    let mut b = Env::new();
    b.insert::<CounterKey>(2);
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
    assert!(child.has::<CounterKey>());
  }

  #[test]
  fn env_get_any_missing() {
    let env = Env::new();
    assert!(env.get_any::<i64>().is_err());
  }
}
