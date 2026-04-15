//! Shared mutable reference — the Effect.ts `Ref<A>`.
//!
//! All mutation operations lock the inner `Mutex` for the minimum window required.
//! The effectful variant [`Ref::modify_effect`] releases the lock *before* awaiting
//! the inner effect so the mutex is never held across an `.await` point.

use std::sync::{Arc, Mutex};

use crate::kernel::{Effect, box_future};

// ── Core type ───────────────────────────────────────────────────────────────

/// Shared mutable cell.  Cloning produces a second handle to the **same** cell.
///
/// Mirrors Effect.ts `Ref<A>`: every operation returns an [`Effect`] value that
/// is lazy — nothing happens until the effect is run.
#[derive(Clone)]
pub struct Ref<A>(Arc<Mutex<A>>);

// ── impl ─────────────────────────────────────────────────────────────────────

impl<A: Clone + Send + 'static> Ref<A> {
  /// `Ref.make(value)` → `Effect<Ref<A>>`
  pub fn make(value: A) -> Effect<Ref<A>> {
    Effect::new(|_r| Ok(Ref(Arc::new(Mutex::new(value)))))
  }

  /// Allocates a cell with `value` without running an [`Effect`] (same inner state as [`Ref::make`]).
  ///
  /// Use this at composition boundaries when you need a [`Ref`] synchronously (e.g. building a
  /// [`Context`](crate::context::Context)) and cannot call [`crate::runtime::run_blocking`].
  #[inline]
  pub fn from_value(value: A) -> Self {
    Self(Arc::new(Mutex::new(value)))
  }

  /// `Ref.get` → `Effect<A>`
  pub fn get(&self) -> Effect<A> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| Ok(inner.lock().expect("Ref: mutex poisoned").clone()))
  }

  /// `Ref.set(value)` → `Effect<()>`
  pub fn set(&self, value: A) -> Effect<()> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      *inner.lock().expect("Ref: mutex poisoned") = value;
      Ok(())
    })
  }

  /// `Ref.update(f)` → `Effect<()>`
  pub fn update(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<()> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      let mut guard = inner.lock().expect("Ref: mutex poisoned");
      let old = guard.clone();
      *guard = f(old);
      Ok(())
    })
  }

  /// `Ref.updateAndGet(f)` → `Effect<A>`
  pub fn update_and_get(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<A> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      let mut guard = inner.lock().expect("Ref: mutex poisoned");
      let new_val = f(guard.clone());
      *guard = new_val.clone();
      Ok(new_val)
    })
  }

  /// `Ref.getAndUpdate(f)` → `Effect<A>` (returns the **old** value)
  pub fn get_and_update(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<A> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      let mut guard = inner.lock().expect("Ref: mutex poisoned");
      let old = guard.clone();
      *guard = f(old.clone());
      Ok(old)
    })
  }

  /// `Ref.modify(f)` → `Effect<B>` where `f: A → (B, A)`
  pub fn modify<B>(&self, f: impl FnOnce(A) -> (B, A) + Send + 'static) -> Effect<B>
  where
    B: Send + 'static,
  {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      let mut guard = inner.lock().expect("Ref: mutex poisoned");
      let (b, new_val) = f(guard.clone());
      *guard = new_val;
      Ok(b)
    })
  }

  /// `Ref.modifyEffect(f)` → `Effect<B, E, R>` where `f` returns an effectful `(B, A)`.
  ///
  /// The mutex is **released** before the inner effect is awaited, so this never holds
  /// a lock across an `.await` point.  Concurrent calls are therefore interleaved, not
  /// serialized — use [`crate::coordination::synchronized_ref::SynchronizedRef`] when you need
  /// strict serialization.
  pub fn modify_effect<B, E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<(B, A), E, R> + Send + 'static,
  ) -> Effect<B, E, R>
  where
    B: Send + 'static,
    E: 'static,
    R: 'static,
  {
    let inner = Arc::clone(&self.0);
    Effect::new_async(move |r: &mut R| {
      // Snapshot the current value *before* the await; the lock is dropped here.
      let old = inner.lock().expect("Ref: mutex poisoned").clone();
      box_future(async move {
        let (b, new_val) = f(old).run(r).await?;
        *inner.lock().expect("Ref: mutex poisoned") = new_val;
        Ok(b)
      })
    })
  }

  /// `Ref.getAndSet(value)` → `Effect<A>` (returns the **old** value)
  pub fn get_and_set(&self, value: A) -> Effect<A> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      let mut guard = inner.lock().expect("Ref: mutex poisoned");
      let old = guard.clone();
      *guard = value;
      Ok(old)
    })
  }

  /// `Ref.setAndGet(value)` → `Effect<A>` (sets then returns the **new** value)
  pub fn set_and_get(&self, value: A) -> Effect<A> {
    let inner = Arc::clone(&self.0);
    Effect::new(move |_r| {
      let mut guard = inner.lock().expect("Ref: mutex poisoned");
      *guard = value.clone();
      Ok(value)
    })
  }
}

// ── Module-level free functions (Effect.ts style) ────────────────────────────

/// `Ref.make(value)` — free-function form.
pub fn make<A: Clone + Send + 'static>(value: A) -> Effect<Ref<A>> {
  Ref::make(value)
}

/// `Ref.get(ref)` — free-function form.
pub fn get<A: Clone + Send + 'static>(r: &Ref<A>) -> Effect<A> {
  r.get()
}

/// `Ref.set(ref, value)` — free-function form.
pub fn set<A: Clone + Send + 'static>(r: &Ref<A>, value: A) -> Effect<()> {
  r.set(value)
}

/// `Ref.update(ref, f)` — free-function form.
pub fn update<A: Clone + Send + 'static>(
  r: &Ref<A>,
  f: impl FnOnce(A) -> A + Send + 'static,
) -> Effect<()> {
  r.update(f)
}

/// `Ref.updateAndGet(ref, f)` — free-function form.
pub fn update_and_get<A: Clone + Send + 'static>(
  r: &Ref<A>,
  f: impl FnOnce(A) -> A + Send + 'static,
) -> Effect<A> {
  r.update_and_get(f)
}

/// `Ref.getAndUpdate(ref, f)` — free-function form.
pub fn get_and_update<A: Clone + Send + 'static>(
  r: &Ref<A>,
  f: impl FnOnce(A) -> A + Send + 'static,
) -> Effect<A> {
  r.get_and_update(f)
}

/// `Ref.modify(ref, f)` — free-function form.
pub fn modify<A, B>(r: &Ref<A>, f: impl FnOnce(A) -> (B, A) + Send + 'static) -> Effect<B>
where
  A: Clone + Send + 'static,
  B: Send + 'static,
{
  r.modify(f)
}

/// `Ref.modifyEffect(ref, f)` — free-function form.
pub fn modify_effect<A, B, E, R>(
  r: &Ref<A>,
  f: impl FnOnce(A) -> Effect<(B, A), E, R> + Send + 'static,
) -> Effect<B, E, R>
where
  A: Clone + Send + 'static,
  B: Send + 'static,
  E: 'static,
  R: 'static,
{
  r.modify_effect(f)
}

/// `Ref.getAndSet(ref, value)` — free-function form.
pub fn get_and_set<A: Clone + Send + 'static>(r: &Ref<A>, value: A) -> Effect<A> {
  r.get_and_set(value)
}

/// `Ref.setAndGet(ref, value)` — free-function form.
pub fn set_and_get<A: Clone + Send + 'static>(r: &Ref<A>, value: A) -> Effect<A> {
  r.set_and_get(value)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_blocking;
  use rstest::rstest;

  // Helper: run an Effect<A, (), ()> and unwrap.
  fn run<A: 'static>(eff: Effect<A>) -> A {
    run_blocking(eff, ()).expect("effect failed in test")
  }

  // Helper: run an Effect<A, E, R> with an explicit env.
  fn run_with<A: 'static, E: 'static, R: 'static>(eff: Effect<A, E, R>, env: R) -> Result<A, E> {
    run_blocking(eff, env)
  }

  // Helper: build a Ref already populated.
  fn ref_of<A: Clone + Send + 'static>(value: A) -> Ref<A> {
    run(Ref::make(value))
  }

  // ── make ─────────────────────────────────────────────────────────────────

  mod make {
    use super::*;

    #[test]
    fn make_with_integer_creates_ref_holding_initial_value() {
      let r = ref_of(42u32);
      assert_eq!(run(r.get()), 42);
    }

    #[test]
    fn make_with_string_creates_ref_holding_initial_value() {
      let r = ref_of("hello".to_string());
      assert_eq!(run(r.get()), "hello");
    }

    #[test]
    fn make_with_zero_creates_ref_holding_zero() {
      let r = ref_of(0i64);
      assert_eq!(run(r.get()), 0);
    }
  }

  // ── get ──────────────────────────────────────────────────────────────────

  mod get {
    use super::*;

    #[rstest]
    #[case::positive(1u32)]
    #[case::zero(0u32)]
    #[case::large(u32::MAX)]
    fn get_returns_current_value(#[case] initial: u32) {
      let r = ref_of(initial);
      assert_eq!(run(r.get()), initial);
    }

    #[test]
    fn get_called_twice_returns_same_value_when_unmodified() {
      let r = ref_of(7u32);
      assert_eq!(run(r.get()), run(r.get()));
    }
  }

  // ── set ──────────────────────────────────────────────────────────────────

  mod set {
    use super::*;

    #[test]
    fn set_overwrites_initial_value() {
      let r = ref_of(1u32);
      run(r.set(99));
      assert_eq!(run(r.get()), 99);
    }

    #[test]
    fn set_to_same_value_is_idempotent() {
      let r = ref_of(5u32);
      run(r.set(5));
      assert_eq!(run(r.get()), 5);
    }

    #[test]
    fn set_to_zero_stores_zero() {
      let r = ref_of(42u32);
      run(r.set(0));
      assert_eq!(run(r.get()), 0);
    }

    #[test]
    fn set_multiple_times_keeps_last_written_value() {
      let r = ref_of(0u32);
      run(r.set(1));
      run(r.set(2));
      run(r.set(3));
      assert_eq!(run(r.get()), 3);
    }
  }

  // ── update ───────────────────────────────────────────────────────────────

  mod update {
    use super::*;

    #[test]
    fn update_increments_value() {
      let r = ref_of(10u32);
      run(r.update(|n| n + 1));
      assert_eq!(run(r.get()), 11);
    }

    #[test]
    fn update_with_identity_leaves_value_unchanged() {
      let r = ref_of(5u32);
      run(r.update(|n| n));
      assert_eq!(run(r.get()), 5);
    }

    #[test]
    fn update_from_zero_produces_expected_result() {
      let r = ref_of(0u32);
      run(r.update(|n| n + 100));
      assert_eq!(run(r.get()), 100);
    }

    #[test]
    fn update_returns_unit() {
      let r = ref_of(1u32);
      run(r.update(|n| n + 1));
    }
  }

  // ── update_and_get ───────────────────────────────────────────────────────

  mod update_and_get {
    use super::*;

    #[test]
    fn update_and_get_returns_new_value() {
      let r = ref_of(10u32);
      let new_val = run(r.update_and_get(|n| n * 2));
      assert_eq!(new_val, 20);
    }

    #[test]
    fn update_and_get_cell_contains_new_value_after_call() {
      let r = ref_of(3u32);
      run(r.update_and_get(|n| n + 7));
      assert_eq!(run(r.get()), 10);
    }

    #[test]
    fn update_and_get_with_identity_returns_same_value() {
      let r = ref_of(42u32);
      let val = run(r.update_and_get(|n| n));
      assert_eq!(val, 42);
    }
  }

  // ── get_and_update ────────────────────────────────────────────────────────

  mod get_and_update {
    use super::*;

    #[test]
    fn get_and_update_returns_old_value() {
      let r = ref_of(10u32);
      let old = run(r.get_and_update(|n| n + 5));
      assert_eq!(old, 10);
    }

    #[test]
    fn get_and_update_cell_contains_new_value_after_call() {
      let r = ref_of(10u32);
      run(r.get_and_update(|n| n + 5));
      assert_eq!(run(r.get()), 15);
    }

    #[test]
    fn get_and_update_with_identity_returns_old_and_keeps_same() {
      let r = ref_of(7u32);
      let old = run(r.get_and_update(|n| n));
      assert_eq!(old, 7);
      assert_eq!(run(r.get()), 7);
    }
  }

  // ── modify ────────────────────────────────────────────────────────────────

  mod modify {
    use super::*;

    #[test]
    fn modify_returns_derived_value_and_updates_cell() {
      let r = ref_of(10u32);
      let extracted = run(r.modify(|n| (n * 3, n + 1)));
      assert_eq!(extracted, 30);
      assert_eq!(run(r.get()), 11);
    }

    #[test]
    fn modify_with_string_extraction_works() {
      let r = ref_of(5u32);
      let label = run(r.modify(|n| (format!("was-{n}"), 0)));
      assert_eq!(label, "was-5");
      assert_eq!(run(r.get()), 0);
    }

    #[test]
    fn modify_atomic_read_modify_write_leaves_cell_at_new_value() {
      let r = ref_of(1u32);
      let _ = run(r.modify(|n| (n, n + 99)));
      assert_eq!(run(r.get()), 100);
    }

    #[rstest]
    #[case::double(4u32, 8u32, 4u32)]
    #[case::zero(0u32, 0u32, 0u32)]
    fn modify_returns_original_and_stores_double(
      #[case] init: u32,
      #[case] expected_b: u32,
      #[case] expected_cell: u32,
    ) {
      let r = ref_of(init);
      let b = run(r.modify(|n| (n * 2, n)));
      assert_eq!(b, expected_b);
      assert_eq!(run(r.get()), expected_cell);
    }
  }

  // ── modify_effect ─────────────────────────────────────────────────────────

  mod modify_effect {
    use super::*;
    use crate::kernel::succeed;

    #[test]
    fn modify_effect_with_sync_inner_effect_updates_cell_and_returns_b() {
      let r = ref_of(10u32);
      let b = run_with(
        r.modify_effect(|n| succeed::<(String, u32), (), ()>((format!("val={n}"), n + 1))),
        (),
      )
      .expect("modify_effect failed");
      assert_eq!(b, "val=10");
      assert_eq!(run(r.get()), 11);
    }

    #[test]
    fn modify_effect_when_inner_effect_succeeds_stores_new_value() {
      let r = ref_of(0u32);
      run_with(
        r.modify_effect(|n| succeed::<((), u32), (), ()>(((), n + 42))),
        (),
      )
      .expect("modify_effect failed");
      assert_eq!(run(r.get()), 42);
    }

    #[test]
    fn modify_effect_when_inner_effect_fails_does_not_update_cell() {
      let r = ref_of(99u32);
      let result = run_with(
        r.modify_effect(|_n| crate::kernel::fail::<((), u32), &'static str, ()>("boom")),
        (),
      );
      assert!(result.is_err());
      assert_eq!(run(r.get()), 99);
    }
  }

  // ── get_and_set ───────────────────────────────────────────────────────────

  mod get_and_set {
    use super::*;

    #[test]
    fn get_and_set_returns_old_value() {
      let r = ref_of(5u32);
      let old = run(r.get_and_set(20));
      assert_eq!(old, 5);
    }

    #[test]
    fn get_and_set_cell_holds_new_value_after_call() {
      let r = ref_of(5u32);
      run(r.get_and_set(20));
      assert_eq!(run(r.get()), 20);
    }

    #[test]
    fn get_and_set_with_same_value_returns_original() {
      let r = ref_of(7u32);
      let old = run(r.get_and_set(7));
      assert_eq!(old, 7);
      assert_eq!(run(r.get()), 7);
    }
  }

  // ── set_and_get ───────────────────────────────────────────────────────────

  mod set_and_get {
    use super::*;

    #[test]
    fn set_and_get_returns_new_value() {
      let r = ref_of(1u32);
      let val = run(r.set_and_get(99));
      assert_eq!(val, 99);
    }

    #[test]
    fn set_and_get_cell_holds_new_value_after_call() {
      let r = ref_of(0u32);
      run(r.set_and_get(77));
      assert_eq!(run(r.get()), 77);
    }

    #[test]
    fn set_and_get_with_zero_returns_zero() {
      let r = ref_of(42u32);
      let val = run(r.set_and_get(0));
      assert_eq!(val, 0);
      assert_eq!(run(r.get()), 0);
    }
  }

  // ── clone (shared cell) ───────────────────────────────────────────────────

  mod clone_shares_cell {
    use super::*;

    #[test]
    fn clone_produces_second_handle_to_same_cell() {
      let r1 = ref_of(10u32);
      let r2 = r1.clone();
      run(r1.set(99));
      assert_eq!(run(r2.get()), 99);
    }

    #[test]
    fn write_through_clone_visible_via_original() {
      let r1 = ref_of(0u32);
      let r2 = r1.clone();
      run(r2.set(42));
      assert_eq!(run(r1.get()), 42);
    }
  }

  // ── free functions ────────────────────────────────────────────────────────

  mod free_functions {
    use super::super::*;
    use crate::runtime::run_blocking;

    fn run<A: 'static>(eff: Effect<A>) -> A {
      run_blocking(eff, ()).expect("effect failed")
    }

    #[test]
    fn free_make_creates_readable_ref() {
      let r = run(make(100u32));
      assert_eq!(run(get(&r)), 100);
    }

    #[test]
    fn free_set_updates_value() {
      let r = run(make(0u32));
      run(set(&r, 55));
      assert_eq!(run(get(&r)), 55);
    }

    #[test]
    fn free_update_applies_function() {
      let r = run(make(5u32));
      run(update(&r, |n| n * 3));
      assert_eq!(run(get(&r)), 15);
    }

    #[test]
    fn free_update_and_get_returns_new_value() {
      let r = run(make(2u32));
      let v = run(update_and_get(&r, |n| n + 8));
      assert_eq!(v, 10);
    }

    #[test]
    fn free_get_and_update_returns_old_value() {
      let r = run(make(10u32));
      let old = run(get_and_update(&r, |n| n - 10));
      assert_eq!(old, 10);
      assert_eq!(run(get(&r)), 0);
    }

    #[test]
    fn free_modify_returns_b_and_updates_cell() {
      let r = run(make(6u32));
      let b = run(modify(&r, |n| (n * 10, n - 1)));
      assert_eq!(b, 60);
      assert_eq!(run(get(&r)), 5);
    }

    #[test]
    fn free_get_and_set_returns_old() {
      let r = run(make(3u32));
      let old = run(get_and_set(&r, 30));
      assert_eq!(old, 3);
      assert_eq!(run(get(&r)), 30);
    }

    #[test]
    fn free_set_and_get_returns_new() {
      let r = run(make(0u32));
      let v = run(set_and_get(&r, 123));
      assert_eq!(v, 123);
    }

    #[test]
    fn free_modify_effect_updates_cell_and_returns_b() {
      use crate::kernel::succeed;
      let r = run(make(10u32));
      let b = run_blocking(
        modify_effect(&r, |n| succeed::<(u32, u32), (), ()>((n * 2, n + 1))),
        (),
      )
      .expect("modify_effect failed");
      assert_eq!(b, 20);
      assert_eq!(run(get(&r)), 11);
    }
  }
}
