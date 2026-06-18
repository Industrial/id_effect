//! Fiber/request-scoped capability overrides via [`Env`] clone overlays.

use super::env::Env;
use super::key::CapabilityKey;
use crate::concurrency::fiber_ref::with_fiber_id;
use crate::runtime::FiberId;
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
  static FIBER_STACKS: RefCell<HashMap<u64, Vec<Env>>> = RefCell::new(HashMap::new());
}

#[inline]
fn fiber_key() -> u64 {
  crate::concurrency::fiber_ref::current_fiber_id().as_u64()
}

fn push_env(env: Env) {
  FIBER_STACKS.with(|stacks| {
    stacks
      .borrow_mut()
      .entry(fiber_key())
      .or_default()
      .push(env);
  });
}

fn pop_env() {
  FIBER_STACKS.with(|stacks| {
    let mut stacks = stacks.borrow_mut();
    if let Some(layer) = stacks.get_mut(&fiber_key()) {
      layer.pop();
      if layer.is_empty() {
        stacks.remove(&fiber_key());
      }
    }
  });
}

/// Borrow the active fiber overlay env when present.
pub fn active_env() -> Option<Env> {
  FIBER_STACKS.with(|stacks| {
    stacks
      .borrow()
      .get(&fiber_key())
      .and_then(|v| v.last())
      .cloned()
  })
}

/// Apply `value` to a clone of `base` and run `f` against the overlay (restored after).
pub fn with_override<K, R, F>(base: &Env, value: K::Value, f: F) -> R
where
  K: CapabilityKey,
  K::Value: Clone + Send + Sync + 'static,
  F: FnOnce(&Env) -> R,
{
  let mut overlay = base.clone();
  overlay.insert::<K>(value);
  push_env(overlay.clone());
  let out = f(&overlay);
  pop_env();
  out
}

/// Like [`with_override`] under an explicit [`FiberId`].
pub fn with_fiber_and_override<K, R, F>(fiber: FiberId, base: &Env, value: K::Value, f: F) -> R
where
  K: CapabilityKey,
  K::Value: Clone + Send + Sync + 'static,
  F: FnOnce(&Env) -> R,
{
  with_fiber_id(fiber, || with_override::<K, R, F>(base, value, f))
}

#[cfg(test)]
#[allow(dead_code, clippy::new_ret_no_self)]
mod tests {
  use super::super::needs::Needs;
  use super::*;

  #[::id_effect::capability(u32)]
  struct Counter;

  #[test]
  fn nested_override_restores_parent() {
    let mut base = Env::new();
    base.insert::<CounterKey>(1u32);
    with_override::<CounterKey, _, _>(&base, 9u32, |env| {
      assert_eq!(*Needs::<CounterKey>::need(env), 9);
      with_override::<CounterKey, _, _>(env, 3u32, |inner| {
        assert_eq!(*Needs::<CounterKey>::need(inner), 3);
      });
      assert_eq!(*Needs::<CounterKey>::need(env), 9);
    });
    assert_eq!(*base.get::<CounterKey>(), 1);
  }
}
