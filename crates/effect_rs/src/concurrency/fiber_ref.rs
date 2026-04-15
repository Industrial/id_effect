//! Fiber-local mutable cells — mirrors Effect.ts `FiberRef`.
//!
//! Values are keyed by `(ref_id, [`FiberId`](crate::FiberId))`. The **current fiber** is read from a
//! [`std::thread_local`] [`FiberId`] (default [`FiberId::ROOT`]). This matches single-threaded
//! [`run_blocking`](crate::runtime::run_blocking) and `tokio::runtime::Builder::new_current_thread`;
//! multi-threaded task migration is not tracked yet.
//!
//! When spawning logical child fibers, call [`FiberRef::on_fork`] then run child work under
//! [`with_fiber_id`], and call [`FiberRef::on_join`] when the child completes.

use std::cell::Cell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::kernel::{Effect, box_future};
use crate::runtime::FiberId;

// ── current fiber (thread-local) ─────────────────────────────────────────────

thread_local! {
  static CURRENT_FIBER: Cell<FiberId> = const { Cell::new(FiberId::ROOT) };
}

/// Run `f` while the thread-local `CURRENT_FIBER` cell is set to `fiber`, then restore the previous id.
#[inline]
pub fn with_fiber_id<R>(fiber: FiberId, f: impl FnOnce() -> R) -> R {
  CURRENT_FIBER.with(|cell| {
    let prev = cell.get();
    cell.set(fiber);
    let out = f();
    cell.set(prev);
    out
  })
}

#[inline]
fn current_fiber_id() -> FiberId {
  CURRENT_FIBER.with(|c| c.get())
}

static NEXT_REF_KEY: AtomicU64 = AtomicU64::new(1);

type Store<A> = Mutex<HashMap<(u64, u64), A>>;

type SharedInit<A> = Arc<dyn Fn() -> A + Send + Sync>;
type SharedFork<A> = Arc<dyn Fn(&A) -> A + Send + Sync>;
type SharedJoin<A> = Arc<dyn Fn(&A, &A) -> A + Send + Sync>;

/// Fiber-local reference: each [`FiberId`] may hold its own value; [`get`](FiberRef::get) lazily
/// inserts [`initial`](FiberRef::make) on first access.
#[derive(Clone)]
pub struct FiberRef<A> {
  key: u64,
  initial: SharedInit<A>,
  fork: SharedFork<A>,
  join: SharedJoin<A>,
  store: Arc<Store<A>>,
}

impl<A> FiberRef<A>
where
  A: Clone + Send + Sync + 'static,
{
  /// Allocate a new fiber ref; first read per fiber uses `initial`.
  #[inline]
  pub fn make<F, R>(initial: F) -> Effect<FiberRef<A>, (), R>
  where
    F: Fn() -> A + Send + Sync + 'static,
    R: 'static,
  {
    Self::make_with(initial, |a| a.clone(), |_p, c| c.clone())
  }

  /// Like [`make`](Self::make) with custom **fork** (parent → child seed) and **join** (parent, child → parent).
  #[inline]
  pub fn make_with<F, FF, JF, R>(initial: F, fork: FF, join: JF) -> Effect<FiberRef<A>, (), R>
  where
    F: Fn() -> A + Send + Sync + 'static,
    FF: Fn(&A) -> A + Send + Sync + 'static,
    JF: Fn(&A, &A) -> A + Send + Sync + 'static,
    R: 'static,
  {
    Effect::new(move |_r| {
      let key = NEXT_REF_KEY.fetch_add(1, Ordering::Relaxed);
      Ok(FiberRef {
        key,
        initial: Arc::new(initial),
        fork: Arc::new(fork),
        join: Arc::new(join),
        store: Arc::new(Mutex::new(HashMap::new())),
      })
    })
  }

  fn slot(&self, fiber: FiberId) -> (u64, u64) {
    (self.key, fiber.as_u64())
  }

  /// Seed the child's slot from the parent's current value using `fork`.
  pub fn on_fork<R>(&self, parent: FiberId, child: FiberId) -> Effect<(), (), R>
  where
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
      let pk = fr.slot(parent);
      if let Entry::Vacant(e) = map.entry(pk) {
        e.insert((fr.initial)());
      }
      let parent_v = map
        .get(&pk)
        .cloned()
        .expect("FiberRef.on_fork: parent value must exist");
      let child_v = (fr.fork)(&parent_v);
      map.insert(fr.slot(child), child_v);
      Ok(())
    })
  }

  /// Merge child's value into the parent with `join`, then remove the child's slot.
  pub fn on_join<R>(&self, parent: FiberId, child: FiberId) -> Effect<(), (), R>
  where
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
      let pk = fr.slot(parent);
      let ck = fr.slot(child);
      let child_v = map.remove(&ck);
      let parent_v = map.get(&pk).cloned();
      match (parent_v, child_v) {
        (Some(p), Some(c)) => {
          let merged = (fr.join)(&p, &c);
          map.insert(pk, merged);
        }
        (None, Some(c)) => {
          map.insert(pk, c);
        }
        _ => {}
      }
      Ok(())
    })
  }

  /// Read (or lazily initialize) this fiber's value.
  pub fn get<R>(&self) -> Effect<A, (), R>
  where
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let fid = current_fiber_id();
      let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
      let k = fr.slot(fid);
      if let Some(v) = map.get(&k) {
        return Ok(v.clone());
      }
      let v = (fr.initial)();
      map.insert(k, v.clone());
      Ok(v)
    })
  }

  /// Overwrite this fiber's value.
  pub fn set<R>(&self, value: A) -> Effect<(), (), R>
  where
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let fid = current_fiber_id();
      fr.store
        .lock()
        .expect("FiberRef: registry mutex poisoned")
        .insert(fr.slot(fid), value);
      Ok(())
    })
  }

  /// Update in place from the current value.
  pub fn update<F, R>(&self, f: F) -> Effect<(), (), R>
  where
    F: FnOnce(A) -> A + Send + 'static,
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let fid = current_fiber_id();
      let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
      let k = fr.slot(fid);
      let cur = map.get(&k).cloned().unwrap_or_else(|| (fr.initial)());
      let next = f(cur);
      map.insert(k, next);
      Ok(())
    })
  }

  /// Replace with `f` returning an output and the next stored value.
  pub fn modify<B, F, R>(&self, f: F) -> Effect<B, (), R>
  where
    B: Send + 'static,
    F: FnOnce(A) -> (B, A) + Send + 'static,
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let fid = current_fiber_id();
      let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
      let k = fr.slot(fid);
      let cur = map.get(&k).cloned().unwrap_or_else(|| (fr.initial)());
      let (out, next) = f(cur);
      map.insert(k, next);
      Ok(out)
    })
  }

  /// Drop this fiber's slot so the next [`get`](Self::get) re-runs [`initial`](Self::make).
  pub fn reset<R>(&self) -> Effect<(), (), R>
  where
    R: 'static,
  {
    let fr = self.clone();
    Effect::new(move |_r| {
      let fid = current_fiber_id();
      fr.store
        .lock()
        .expect("FiberRef: registry mutex poisoned")
        .remove(&fr.slot(fid));
      Ok(())
    })
  }

  /// Override the value for `inner` only, then restore the previous slot state for this fiber.
  pub fn locally<B, E, REnv>(&self, value: A, inner: Effect<B, E, REnv>) -> Effect<B, E, REnv>
  where
    B: 'static,
    E: 'static,
    REnv: 'static,
  {
    let fr = self.clone();
    Effect::new_async(move |r| {
      let fid = current_fiber_id();
      let k = fr.slot(fid);
      let previous = fr
        .store
        .lock()
        .expect("FiberRef: registry mutex poisoned")
        .remove(&k);
      fr.store
        .lock()
        .expect("FiberRef: registry mutex poisoned")
        .insert(k, value);
      box_future(async move {
        let result = inner.run(r).await;
        let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
        map.remove(&k);
        if let Some(v) = previous {
          map.insert(k, v);
        }
        result
      })
    })
  }

  /// Like [`locally`](Self::locally) but the override is computed with `f` at entry.
  pub fn locally_with<B, E, REnv, F>(&self, f: F, inner: Effect<B, E, REnv>) -> Effect<B, E, REnv>
  where
    F: FnOnce() -> A + Send + 'static,
    B: 'static,
    E: 'static,
    REnv: 'static,
  {
    let fr = self.clone();
    Effect::new_async(move |r| {
      let value = f();
      let fid = current_fiber_id();
      let k = fr.slot(fid);
      let previous = fr
        .store
        .lock()
        .expect("FiberRef: registry mutex poisoned")
        .remove(&k);
      fr.store
        .lock()
        .expect("FiberRef: registry mutex poisoned")
        .insert(k, value);
      box_future(async move {
        let result = inner.run(r).await;
        let mut map = fr.store.lock().expect("FiberRef: registry mutex poisoned");
        map.remove(&k);
        if let Some(v) = previous {
          map.insert(k, v);
        }
        result
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::kernel::succeed;
  use crate::runtime::run_blocking;

  #[test]
  fn fiber_ref_locally_restores_after_scope() {
    let fr = run_blocking(FiberRef::make(|| 5u32), ()).unwrap();
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 5);
    let inner = fr.clone().locally(99, {
      let g = fr.clone();
      g.get().flat_map(|v| {
        assert_eq!(v, 99u32);
        succeed(())
      })
    });
    assert!(run_blocking(inner, ()).is_ok());
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 5);
  }

  #[test]
  fn fiber_ref_fork_clones_value() {
    let fr = run_blocking(FiberRef::make(|| 0u32), ()).unwrap();
    let child = FiberId::fresh();
    with_fiber_id(FiberId::ROOT, || {
      run_blocking(fr.set(10), ()).unwrap();
      run_blocking(fr.on_fork(FiberId::ROOT, child), ()).unwrap();
    });
    with_fiber_id(child, || {
      assert_eq!(run_blocking(fr.get(), ()).unwrap(), 10);
    });
  }

  #[test]
  fn fiber_ref_join_applies_join_fn() {
    let fr = run_blocking(FiberRef::make_with(|| 0u32, |a| *a, |p, c| p + c), ()).unwrap();
    let child = FiberId::fresh();
    with_fiber_id(FiberId::ROOT, || {
      run_blocking(fr.set(5), ()).unwrap();
      run_blocking(fr.on_fork(FiberId::ROOT, child), ()).unwrap();
    });
    with_fiber_id(child, || {
      run_blocking(fr.set(3), ()).unwrap();
    });
    with_fiber_id(FiberId::ROOT, || {
      run_blocking(fr.on_join(FiberId::ROOT, child), ()).unwrap();
      assert_eq!(run_blocking(fr.get(), ()).unwrap(), 8);
    });
  }

  #[test]
  fn fiber_ref_reset_restores_initial() {
    let fr = run_blocking(FiberRef::make(|| 7u32), ()).unwrap();
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 7);
    run_blocking(fr.set(9), ()).unwrap();
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 9);
    run_blocking(fr.reset(), ()).unwrap();
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 7);
  }

  // ── update ────────────────────────────────────────────────────────────────

  #[test]
  fn fiber_ref_update_applies_function_to_current_value() {
    let fr = run_blocking(FiberRef::make(|| 10u32), ()).unwrap();
    run_blocking(fr.update(|v| v * 2), ()).unwrap();
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 20);
  }

  #[test]
  fn fiber_ref_update_uses_initial_when_no_value_set() {
    let fr = run_blocking(FiberRef::make(|| 5u32), ()).unwrap();
    // Reset first so no value is set, then update
    run_blocking(fr.reset(), ()).unwrap();
    run_blocking(fr.update(|v| v + 1), ()).unwrap();
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 6);
  }

  // ── modify ────────────────────────────────────────────────────────────────

  #[test]
  fn fiber_ref_modify_returns_output_and_stores_new_value() {
    let fr = run_blocking(FiberRef::make(|| 100u32), ()).unwrap();
    let out = run_blocking(fr.modify(|v| (v.to_string(), v + 1)), ()).unwrap();
    assert_eq!(out, "100");
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 101);
  }

  #[test]
  fn fiber_ref_modify_uses_initial_when_unset() {
    let fr = run_blocking(FiberRef::make(|| 50u32), ()).unwrap();
    run_blocking(fr.reset(), ()).unwrap();
    let out = run_blocking(fr.modify(|v| (v * 2, v + 10)), ()).unwrap();
    assert_eq!(out, 100u32);
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 60);
  }

  // ── locally_with ──────────────────────────────────────────────────────────

  #[test]
  fn fiber_ref_locally_with_computes_override_at_entry_and_restores() {
    let fr = run_blocking(FiberRef::make(|| 3u32), ()).unwrap();
    run_blocking(fr.set(3), ()).unwrap();
    let inner = fr.clone().locally_with(|| 42u32, {
      let g = fr.clone();
      g.get().flat_map(|v| {
        assert_eq!(v, 42u32);
        succeed(())
      })
    });
    assert!(run_blocking(inner, ()).is_ok());
    // Original value should be restored
    assert_eq!(run_blocking(fr.get(), ()).unwrap(), 3);
  }
}
