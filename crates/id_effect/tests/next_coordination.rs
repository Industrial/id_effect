//! `c-coordination` acceptance tests for `id_effect::next` coordination primitives.
//!
//! The v1 unit tests of the root `coordination` module (and `concurrency::supervisor`) are
//! ported here against the next kernel, plus interruption tests: an interrupted waiter is removed
//! from its primitive and never leaks a permit or an item.
//!
//! Most tests run on the global runtime through `block_on`; the interruption races run on the
//! deterministic `TestExecutor` so the order of hand-over and interrupt is fixed.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::next::{
  Effect, FiberHandle, Ref, Runtime, Semaphore, SynchronizedRef, TestExecutor, fail, sleep,
  succeed, suspend, sync, yield_fiber,
};
use id_effect::runtime::Never;
use id_effect::{Cause, Exit};

// ── helpers ───────────────────────────────────────────────────────────────────────────────

/// Runs `eff` on the global runtime and returns its success value.
fn run<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("effect failed: {c:?}"),
  }
}

/// Runs `eff` on the global runtime and returns its exit.
fn run_exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  eff.block_on()
}

/// Polls `fut` once; it must be ready.
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

/// Spawns `eff` as a root fiber on a fresh `TestExecutor` and runs it until it stalls.
fn spawn_test<A: Send + 'static, E: Send + 'static>(
  eff: Effect<A, E>,
) -> (TestExecutor, FiberHandle<A, E>) {
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  (exec, h)
}

/// Yields `n` times (lets forked fibers reach their suspension point on a real executor).
fn yields(n: usize) -> Effect<(), Never> {
  suspend(move || {
    if n == 0 {
      succeed(())
    } else {
      yield_fiber().flat_map(move |()| yields(n - 1))
    }
  })
}

// ── Ref (ported from v1 `coordination::ref_`) ─────────────────────────────────────────────

mod ref_ {
  use super::*;

  fn ref_of<A: Send + 'static>(a: A) -> Ref<A> {
    Ref::new(a)
  }

  #[rstest::rstest]
  #[case::positive(1u32)]
  #[case::zero(0u32)]
  #[case::large(u32::MAX)]
  fn get_returns_current_value(#[case] initial: u32) {
    assert_eq!(run(ref_of(initial).get()), initial);
  }

  #[test]
  fn new_with_integer_string_and_zero_holds_initial_value() {
    assert_eq!(run(ref_of(42u32).get()), 42);
    assert_eq!(run(ref_of("hello".to_string()).get()), "hello");
    assert_eq!(run(ref_of(0i64).get()), 0);
  }

  #[test]
  fn get_called_twice_returns_same_value_when_unmodified() {
    let r = ref_of(7u32);
    assert_eq!(run(r.get()), run(r.get()));
  }

  #[test]
  fn set_overwrites_and_keeps_last_written_value() {
    let r = ref_of(1u32);
    run(r.set(99));
    assert_eq!(run(r.get()), 99);
    run(r.set(99));
    assert_eq!(run(r.get()), 99, "idempotent");
    run(r.set(0));
    assert_eq!(run(r.get()), 0);
    for v in [3u32, 5, 8] {
      run(r.set(v));
    }
    assert_eq!(run(r.get()), 8);
  }

  #[test]
  fn update_applies_function() {
    let r = ref_of(0u32);
    run(r.update(|n| n + 1));
    assert_eq!(run(r.get()), 1);
    run(r.update(|n| n));
    assert_eq!(run(r.get()), 1, "identity leaves the value unchanged");
    let unit: () = run(r.update(|n| n * 10));
    assert_eq!(unit, ());
    assert_eq!(run(r.get()), 10);
  }

  #[test]
  fn update_and_get_returns_new_value() {
    let r = ref_of(5u32);
    assert_eq!(run(r.update_and_get(|n| n * 2)), 10);
    assert_eq!(run(r.get()), 10);
    assert_eq!(run(r.update_and_get(|n| n)), 10);
  }

  #[test]
  fn get_and_update_returns_old_value() {
    let r = ref_of(5u32);
    assert_eq!(run(r.get_and_update(|n| n + 1)), 5);
    assert_eq!(run(r.get()), 6);
    assert_eq!(run(r.get_and_update(|n| n)), 6);
    assert_eq!(run(r.get()), 6);
  }

  #[rstest::rstest]
  #[case::double(4u32, 8u32, 4u32)]
  #[case::zero(0u32, 0u32, 0u32)]
  fn modify_returns_derived_value_and_stores_new(
    #[case] init: u32,
    #[case] expected_b: u32,
    #[case] expected_cell: u32,
  ) {
    let r = ref_of(init);
    assert_eq!(run(r.modify(|n| (n * 2, n))), expected_b);
    assert_eq!(run(r.get()), expected_cell);
  }

  #[test]
  fn modify_with_string_extraction_and_atomic_read_modify_write() {
    let r = ref_of("abc".to_string());
    let len = run(r.modify(|s| (s.len(), format!("{s}!"))));
    assert_eq!(len, 3);
    assert_eq!(run(r.get()), "abc!");

    let counter = ref_of(0u64);
    let incs: Vec<Effect<(), Never>> = (0..100).map(|_| counter.modify(|n| ((), n + 1))).collect();
    let all = incs.into_iter().fold(succeed(()), |acc, e| acc.then(e));
    run(all);
    assert_eq!(run(counter.get()), 100);
  }

  #[test]
  fn modify_effect_updates_cell_and_returns_b() {
    let r = ref_of(10u32);
    let b = run(r.modify_effect(|n| succeed((format!("val={n}"), n + 1))));
    assert_eq!(b, "val=10");
    assert_eq!(run(r.get()), 11);
    run(r.modify_effect(|n| succeed(((), n + 42))));
    assert_eq!(run(r.get()), 53);
  }

  #[test]
  fn modify_effect_when_inner_effect_fails_does_not_update_cell() {
    let r = ref_of(99u32);
    let out = run_exit(r.modify_effect(|_n| fail::<((), u32), &'static str, _>("boom")));
    assert!(matches!(out, Exit::Failure(Cause::Fail("boom"))));
    assert_eq!(run(r.get()), 99);
  }

  #[test]
  fn get_and_set_and_set_and_get() {
    let r = ref_of(5u32);
    assert_eq!(run(r.get_and_set(9)), 5);
    assert_eq!(run(r.get()), 9);
    assert_eq!(run(r.get_and_set(9)), 9, "same value returns the original");
    assert_eq!(run(r.set_and_get(3)), 3);
    assert_eq!(run(r.get()), 3);
    assert_eq!(run(r.set_and_get(0)), 0);
  }

  #[test]
  fn clone_shares_the_same_cell() {
    let r = ref_of(1u32);
    let r2 = r.clone();
    run(r2.set(2));
    assert_eq!(
      run(r.get()),
      2,
      "write through the clone is visible via the original"
    );
    assert_eq!(format!("{r:?}"), "Ref(2)");
  }

  #[test]
  fn ref_is_send_and_sync_and_ops_are_lazy() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Ref<u32>>();
    let r = ref_of(1u32);
    let eff: Effect<(), Never> = r.set(2);
    assert_eq!(run(r.get()), 1, "nothing happens until the effect runs");
    run(eff);
    assert_eq!(run(r.get()), 2);
  }
}

// ── Semaphore (ported from v1 `coordination::semaphore`) ──────────────────────────────────

mod semaphore {
  use super::*;
  use id_effect::next::{ExitKind, Scope};

  #[test]
  fn try_acquire_none_when_exhausted_some_when_available() {
    let empty = Semaphore::new(0);
    assert!(run(empty.try_acquire()).is_none());
    let sem = Semaphore::new(1);
    let p = run(sem.try_acquire()).expect("permit");
    assert_eq!(p.permits(), 1);
    assert!(run(sem.try_acquire()).is_none());
    drop(p);
    assert!(run(sem.try_acquire()).is_some());
  }

  #[test]
  fn acquire_returns_an_owned_permit_released_on_drop() {
    let sem = Semaphore::new(1);
    let permit = run(sem.acquire());
    assert_eq!(run(sem.available()), 0, "permit consumes the slot");
    permit.release();
    assert_eq!(run(sem.available()), 1, "release restores the slot");
  }

  #[test]
  fn available_tracks_permits() {
    let sem = Semaphore::new(3);
    assert_eq!(run(sem.available()), 3);
    let p = run(sem.acquire());
    assert_eq!(run(sem.available()), 2);
    let p2 = run(sem.acquire_n(2));
    assert_eq!(p2.permits(), 2);
    assert_eq!(run(sem.available()), 0);
    drop((p, p2));
    assert_eq!(run(sem.available()), 3);
    assert_eq!(run(sem.acquire_n(0)).permits(), 0);
    assert!(format!("{sem:?}").contains("available: 3"));
  }

  #[test]
  fn permit_released_on_scope_close() {
    let sem = Semaphore::new(1);
    let scope = Scope::new();
    run(sem.acquire_scoped(&scope));
    assert!(run(sem.try_acquire()).is_none());
    run(scope.close(ExitKind::Success));
    assert!(run(sem.try_acquire()).is_some());
    // On a closed scope the permit is released at once.
    run(sem.acquire_scoped(&scope));
    assert_eq!(run(sem.available()), 1);
  }

  #[test]
  fn acquire_suspends_until_a_permit_is_released() {
    let sem = Semaphore::new(1);
    let held = run(sem.acquire());
    let order = Ref::new(Vec::<&'static str>::new());
    let o = order.clone();
    let waiter = Runtime::global().fork(sem.with_permit(o.update::<Never, _>(|mut v| {
      v.push("waiter");
      v
    })));
    std::thread::sleep(Duration::from_millis(50));
    assert!(
      !waiter.is_done(),
      "the waiter is suspended while the permit is held"
    );
    run(order.update(|mut v| {
      v.push("holder-out");
      v
    }));
    drop(held);
    run(waiter.join());
    assert_eq!(run(order.get()), vec!["holder-out", "waiter"]);
  }

  #[test]
  fn with_permits_bounds_concurrency() {
    let sem = Semaphore::new(2);
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let task = |sem: &Semaphore| {
      let (live, peak, live2) = (Arc::clone(&live), Arc::clone(&peak), Arc::clone(&live));
      sem.with_permit(
        sync(move || {
          let n = live.fetch_add(1, Ordering::SeqCst) + 1;
          peak.fetch_max(n, Ordering::SeqCst);
        })
        .then(sleep(Duration::from_millis(10)))
        .then(sync(move || {
          live2.fetch_sub(1, Ordering::SeqCst);
        })),
      )
    };
    let tasks: Vec<Effect<(), Never>> = (0..8).map(|_| task(&sem)).collect();
    run(id_effect::next::fork_all(tasks, id_effect::next::Concurrency::Unbounded).map(|_| ()));
    assert!(
      peak.load(Ordering::SeqCst) <= 2,
      "at most two holders at once"
    );
    assert_eq!(run(sem.available()), 2, "every permit came back");
  }

  #[test]
  fn with_permits_releases_on_failure_and_defect() {
    let sem = Semaphore::new(1);
    let out = run_exit(sem.with_permit(fail::<(), _, _>("nope")));
    assert!(matches!(out, Exit::Failure(Cause::Fail("nope"))));
    assert_eq!(run(sem.available()), 1);
    let out = run_exit(sem.with_permit(sync::<(), Never, _>(|| panic!("boom"))));
    assert!(matches!(out, Exit::Failure(Cause::Die(_))));
    assert_eq!(run(sem.available()), 1);
  }

  #[test]
  fn fifo_large_waiter_blocks_later_small_ones() {
    let sem = Semaphore::new(2);
    let held = run(sem.acquire());
    let order = Ref::new(Vec::<u8>::new());
    let (s1, s2, o1, o2) = (sem.clone(), sem.clone(), order.clone(), order.clone());
    let big = s1.with_permits(
      2,
      o1.update(|mut v| {
        v.push(2);
        v
      }),
    );
    let small = s2.with_permit(o2.update(|mut v| {
      v.push(1);
      v
    }));
    let (exec, h) = spawn_test(big.fork().flat_map(move |hb| {
      yields(3)
        .then(small.fork())
        .flat_map(move |hs| hb.join().then(hs.join()))
    }));
    assert!(!h.is_done());
    assert!(
      run(order.get()).is_empty(),
      "the small waiter queues behind the big one"
    );
    drop(held);
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(run(order.get()), vec![2, 1]);
  }

  #[test]
  fn interrupted_waiter_is_removed_and_leaks_nothing() {
    let sem = Semaphore::new(1);
    let held = run(sem.acquire());
    let s = sem.clone();
    let (exec, waiter) = spawn_test(s.acquire::<Never, _>().map(drop));
    assert!(!waiter.is_done(), "waiter is suspended");
    waiter.interrupt_now();
    exec.run_until_stalled();
    assert!(matches!(
      take_ready(waiter.join_future()),
      Exit::Failure(Cause::Interrupt(_))
    ));
    drop(held);
    assert_eq!(run(sem.available()), 1, "no permit leaked");
    // `try_acquire` refuses when a waiter is queued ahead: the interrupted one is gone.
    assert!(run(sem.try_acquire()).is_some());
  }

  #[test]
  fn interrupt_after_hand_over_gives_the_permit_back() {
    let sem = Semaphore::new(1);
    let held = run(sem.acquire());
    let s = sem.clone();
    let (exec, waiter) = spawn_test(s.acquire::<Never, _>().map(drop));
    assert!(!waiter.is_done());
    // Release hands the permit to the queued waiter, then the waiter is interrupted before it
    // runs again: the runloop drops the step without polling it, and the permit comes back.
    drop(held);
    waiter.interrupt_now();
    exec.run_until_stalled();
    assert!(matches!(
      take_ready(waiter.join_future()),
      Exit::Failure(Cause::Interrupt(_))
    ));
    assert_eq!(run(sem.available()), 1, "granted permit returned");
  }

  #[test]
  fn interrupting_with_permits_releases_the_held_permit() {
    let sem = Semaphore::new(1);
    let s = sem.clone();
    let (exec, h) = spawn_test(s.with_permit(sleep::<Never, _>(Duration::from_secs(3600))));
    assert_eq!(run(sem.available()), 0, "held while sleeping");
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(run(sem.available()), 1);
  }
}

// ── SynchronizedRef (ported from v1 `coordination::synchronized_ref`) ─────────────────────

mod synchronized_ref {
  use super::*;

  #[test]
  fn update_effect_is_sequential_under_concurrency() {
    let s = SynchronizedRef::new(0_i32);
    let bump = |s: &SynchronizedRef<i32>| {
      s.update_effect(|n| sleep(Duration::from_millis(15)).map(move |()| n + 1))
    };
    let tasks: Vec<Effect<(), Never>> = vec![bump(&s), bump(&s), bump(&s)];
    run(id_effect::next::fork_all(tasks, id_effect::next::Concurrency::Unbounded).map(|_| ()));
    assert_eq!(run(s.get()), 3, "no lost update");
  }

  #[test]
  fn get_set_returns_current() {
    let s = SynchronizedRef::new(10_u32);
    assert_eq!(run(s.get()), 10);
    run(s.set(20));
    assert_eq!(run(s.get()), 20);
  }

  #[test]
  fn modify_returns_computed_value() {
    let s = SynchronizedRef::new(3_i64);
    assert_eq!(run(s.modify(|n| (n * 2, n + 1))), 6);
    assert_eq!(run(s.get()), 4);
  }

  #[test]
  fn update_applies_fn_in_place() {
    let s = SynchronizedRef::new(5_u32);
    run(s.update(|n| n * 3));
    assert_eq!(run(s.get()), 15);
  }

  #[test]
  fn modify_effect_returns_computed_value() {
    let s = SynchronizedRef::new(10_i32);
    assert_eq!(run(s.modify_effect(|n| succeed((n * 2, n + 5)))), 20);
    assert_eq!(run(s.get()), 15);
  }

  #[test]
  fn get_effect_applies_fn_and_leaves_state() {
    let s = SynchronizedRef::new(7_u32);
    assert_eq!(run(s.get_effect(|n| succeed(n * 2))), 14);
    assert_eq!(run(s.get()), 7);
  }

  #[test]
  fn failed_update_effect_leaves_state_and_unlocks() {
    let s = SynchronizedRef::new(1_u32);
    let out = run_exit(s.update_effect(|_| fail::<u32, _, _>("no")));
    assert!(matches!(out, Exit::Failure(Cause::Fail("no"))));
    assert_eq!(run(s.get()), 1, "unchanged, and the lock was released");
    assert!(format!("{s:?}").contains("SynchronizedRef"));
  }

  #[test]
  fn interrupted_update_effect_releases_the_lock() {
    let s = SynchronizedRef::new(1_u32);
    let s2 = s.clone();
    let (exec, h) = spawn_test(
      s2.update_effect::<Never, _>(|n| sleep(Duration::from_secs(3600)).map(move |()| n + 1)),
    );
    assert!(!h.is_done());
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(h.is_done());
    assert_eq!(run(s.get()), 1, "unchanged and unlocked");
  }
}
