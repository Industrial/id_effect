//! `c-coordination` tests for `id_effect::{Deferred, Latch}`: the v1 `deferred` and
//! `latch` unit tests ported to the 0.7 kernel, plus interruption (an interrupted waiter is
//! removed from the wait list).

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Deferred, Effect, FiberHandle, Latch, Runtime, TestExecutor, fail, succeed, suspend, yield_fiber,
};

fn run<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("effect failed: {c:?}"),
  }
}

fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

fn spawn_test<A: Send + 'static, E: Send + 'static>(
  eff: Effect<A, E>,
) -> (TestExecutor, FiberHandle<A, E>) {
  let exec = TestExecutor::new();
  let h = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  (exec, h)
}

fn yields<E: Send + 'static>(n: usize) -> Effect<(), E> {
  suspend(move || {
    if n == 0 {
      succeed(())
    } else {
      yield_fiber().flat_map(move |()| yields(n - 1))
    }
  })
}

// ── Deferred ──────────────────────────────────────────────────────────────────────────────

mod deferred {
  use super::*;

  #[test]
  fn wait_suspends_until_succeed() {
    let d = Deferred::<u8, Never>::new();
    let d2 = d.clone();
    let eff = d
      .wait()
      .fork()
      .flat_map(move |h| yields(3).then(d2.succeed(7)).then(h.join()));
    assert_eq!(run(eff), 7);
  }

  #[test]
  fn second_succeed_returns_false() {
    let d = Deferred::<i32, ()>::new();
    assert!(run(d.succeed(1)));
    assert!(!run(d.succeed(2)));
    assert!(matches!(d.wait().block_on(), Exit::Success(1)));
  }

  #[test]
  fn fail_propagates_error() {
    let d = Deferred::<u8, &'static str>::new();
    assert!(run(d.fail("boom")));
    assert!(matches!(
      d.wait().block_on(),
      Exit::Failure(Cause::Fail("boom"))
    ));
  }

  #[test]
  fn interrupt_delivers_interrupt_cause() {
    let d = Deferred::<u8, ()>::new();
    assert!(run(d.interrupt()));
    assert!(matches!(
      d.wait().block_on(),
      Exit::Failure(Cause::Interrupt(_))
    ));
  }

  #[test]
  fn die_delivers_defect() {
    let d = Deferred::<u8, ()>::new();
    assert!(run(d.die("bad")));
    let Exit::Failure(Cause::Die(defect)) = d.wait().block_on() else {
      panic!("expected a defect")
    };
    assert_eq!(defect.message(), "bad");
  }

  #[test]
  fn poll_none_before_complete() {
    let d = Deferred::<u16, ()>::new();
    assert!(run(d.poll()).is_none());
    assert!(!run(d.is_done()));
    run(d.succeed(42));
    assert!(matches!(run(d.poll()), Some(Exit::Success(42))));
    assert!(run(d.is_done()));
  }

  #[test]
  fn second_fail_cause_returns_false() {
    let d = Deferred::<(), ()>::new();
    assert!(run(d.fail_cause(Cause::die("a"))));
    assert!(!run(d.fail_cause(Cause::die("b"))));
  }

  #[test]
  fn complete_runs_effect_and_stores_its_outcome() {
    let d = Deferred::<u8, ()>::new();
    assert!(run(d.complete(succeed::<u8, (), _>(9))));
    assert!(matches!(d.wait().block_on(), Exit::Success(9)));
    let e = Deferred::<u8, &'static str>::new();
    assert!(run(e.complete(fail::<u8, &'static str, _>("x"))));
    assert!(matches!(
      e.wait().block_on(),
      Exit::Failure(Cause::Fail("x"))
    ));
    assert!(!run(e.complete(succeed(1))), "already completed");
  }

  #[test]
  fn unsafe_done_overwrites_prior() {
    let d = Deferred::<u8, ()>::new();
    run(d.succeed(1));
    run(d.unsafe_done(Exit::Success(2)));
    assert!(matches!(d.wait().block_on(), Exit::Success(2)));
    run(d.done(Exit::Success(3)));
    assert!(
      matches!(d.wait().block_on(), Exit::Success(2)),
      "done keeps the first"
    );
  }

  #[test]
  fn try_succeed_and_try_fail_cause_outside_effects() {
    let d = Deferred::<u8, ()>::default();
    assert!(d.try_succeed(5));
    assert!(!d.try_fail_cause(Cause::fail(())));
    let out = Runtime::global()
      .block_on_future(d.wait_future())
      .expect("not in an async context");
    assert!(matches!(out, Exit::Success(5)));
  }

  #[test]
  fn every_waiter_is_woken() {
    let d = Deferred::<u8, Never>::new();
    let d2 = d.clone();
    let waiters: Vec<Effect<FiberHandle<u8, Never>, Never>> =
      (0..4).map(|_| d.wait().fork()).collect();
    let eff =
      id_effect::fork_all(waiters, id_effect::Concurrency::Sequential).flat_map(move |hs| {
        yields(3).then(d2.succeed(1)).then(id_effect::fork_all(
          hs.into_iter().map(FiberHandle::join).collect(),
          id_effect::Concurrency::Sequential,
        ))
      });
    assert_eq!(run(eff), vec![1, 1, 1, 1]);
  }

  #[test]
  fn interrupted_waiter_is_removed() {
    let d = Deferred::<u8, Never>::new();
    let (exec, h) = spawn_test(d.wait());
    assert!(format!("{d:?}").contains("waiters: 1"), "{d:?}");
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(matches!(
      take_ready(h.join_future()),
      Exit::Failure(Cause::Interrupt(_))
    ));
    assert!(format!("{d:?}").contains("waiters: 0"), "{d:?}");
    assert!(d.try_succeed(1));
  }
}

// ── Latch ─────────────────────────────────────────────────────────────────────────────────

mod latch {
  use super::*;

  #[test]
  fn wait_returns_after_open() {
    let l = Latch::new();
    let l2 = l.clone();
    let eff = l
      .wait::<Never, _>()
      .fork()
      .flat_map(move |h| yields(3).then(l2.open()).then(h.join()));
    run(eff);
    assert!(run(l.is_open()));
  }

  #[test]
  fn open_is_idempotent() {
    let l = Latch::default();
    run(l.open());
    run(l.open());
    assert!(run(l.is_open()));
    run(l.wait());
  }

  #[test]
  fn starts_closed_or_open_and_wait_returns_at_once_when_open() {
    let l = Latch::with_state(true);
    assert!(run(l.is_open()));
    run(l.wait());
    assert!(!run(Latch::new().is_open()));
  }

  #[test]
  fn close_makes_later_waits_suspend_and_release_wakes_current_waiters() {
    let l = Latch::with_state(true);
    run(l.close());
    assert!(!run(l.is_open()));
    let (exec, h) = spawn_test(l.wait::<Never, _>());
    assert!(!h.is_done(), "closed latch suspends");
    run(l.release());
    exec.run_until_stalled();
    assert!(h.is_done(), "released waiter resumes");
    assert!(!run(l.is_open()), "release does not open");
    let (_exec, h2) = spawn_test(l.wait::<Never, _>());
    assert!(!h2.is_done(), "a later waiter still waits");
  }

  #[test]
  fn when_open_runs_after_open() {
    let l = Latch::new();
    let (exec, h) = spawn_test(l.when_open(succeed::<u8, Never, _>(4)));
    assert!(!h.is_done());
    run(l.open());
    exec.run_until_stalled();
    assert!(matches!(take_ready(h.join_future()), Exit::Success(4)));
  }

  #[test]
  fn interrupted_waiter_is_removed() {
    let l = Latch::new();
    let (exec, h) = spawn_test(l.wait::<Never, _>());
    assert!(format!("{l:?}").contains("waiters: 1"), "{l:?}");
    h.interrupt_now();
    exec.run_until_stalled();
    assert!(h.is_done());
    assert!(format!("{l:?}").contains("waiters: 0"), "{l:?}");
  }
}
