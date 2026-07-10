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
  use super::super::key::Cap;
  use super::super::needs::Needs;
  use super::*;
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  struct Counter(pub u32);

  #[test]
  fn nested_override_restores_parent() {
    let mut base = Env::new();
    base.insert::<Cap<Counter>>(Counter(1));
    with_override::<Cap<Counter>, _, _>(&base, Counter(9), |env| {
      assert_eq!(Needs::<Counter>::need(env).0, 9);
      with_override::<Cap<Counter>, _, _>(env, Counter(3), |inner| {
        assert_eq!(Needs::<Counter>::need(inner).0, 3);
      });
      assert_eq!(Needs::<Counter>::need(env).0, 9);
    });
    assert_eq!(base.get::<Cap<Counter>>().0, 1);
  }
  #[test]
  fn active_env_returns_none_without_overlay() {
    assert!(active_env().is_none());
  }

  #[test]
  fn active_env_sees_pushed_overlay() {
    let mut base = Env::new();
    base.insert::<Cap<Counter>>(Counter(1));
    with_override::<Cap<Counter>, _, _>(&base, Counter(5), |_env| {
      let active = active_env().expect("overlay");
      assert_eq!(active.get::<Cap<Counter>>().0, 5);
    });
    assert!(active_env().is_none());
  }

  #[test]
  fn with_fiber_and_override_scopes_to_fiber() {
    let mut base = Env::new();
    base.insert::<Cap<Counter>>(Counter(0));
    let fid = FiberId::new(99);
    let got = with_fiber_and_override::<Cap<Counter>, _, _>(fid, &base, Counter(7), |env| {
      Needs::<Counter>::need(env).0
    });
    assert_eq!(got, 7);
  }
}
