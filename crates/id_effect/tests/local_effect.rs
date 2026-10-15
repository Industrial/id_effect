//! `LocalEffect` combinators on ready values and on erased programs, the cooperative yield
//! inside a continuation, and the `Debug` views of the effect and of its run future.

use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use id_effect::{Cause, Executor, Exit, FiberContext, LocalEffect, Never, TestExecutor};

/// Polls `fut` once; it must be ready.
fn take_ready<F: Future>(fut: F) -> F::Output {
  let mut fut = pin!(fut);
  match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(v) => v,
    Poll::Pending => panic!("future is not ready"),
  }
}

fn panics() -> u8 {
  panic!("boom")
}

#[test]
fn ready_values_short_circuit_flat_map_and_catch() {
  let bound: LocalEffect<u8, &str> =
    LocalEffect::succeed(2).flat_map(|n| LocalEffect::succeed(n * 3));
  assert!(matches!(bound.block_on(), Exit::Success(6)));

  let skipped: LocalEffect<u8, &str> =
    LocalEffect::fail("no").flat_map(|n: u8| LocalEffect::succeed(n + 1));
  assert!(matches!(
    skipped.block_on(),
    Exit::Failure(Cause::Fail("no"))
  ));

  let kept: LocalEffect<u8, ()> =
    LocalEffect::<u8, &str>::succeed(1).catch(|_| LocalEffect::succeed(0));
  assert!(matches!(kept.block_on(), Exit::Success(1)));

  let recovered: LocalEffect<usize, ()> =
    LocalEffect::<usize, &str>::fail("four").catch(|e| LocalEffect::succeed(e.len()));
  assert!(matches!(recovered.block_on(), Exit::Success(4)));

  let defect: LocalEffect<u8, ()> =
    LocalEffect::<u8, &str>::die("broken").catch(|_| LocalEffect::succeed(0));
  assert!(matches!(defect.block_on(), Exit::Failure(Cause::Die(_))));
}

#[test]
fn programs_fold_successes_and_pass_defects_through_catch() {
  // `Rc` keeps the bodies `!Send`, which is what `LocalEffect` is for.
  let shared = Rc::new(5_u8);
  let s = Rc::clone(&shared);
  let mapped: LocalEffect<u8, String> =
    LocalEffect::<u8, &str>::sync(move || *s).map_err(str::to_owned);
  assert!(matches!(mapped.block_on(), Exit::Success(5)));

  let s = Rc::clone(&shared);
  let kept: LocalEffect<u8, ()> =
    LocalEffect::<u8, &str>::sync(move || *s + 1).catch(|_| LocalEffect::succeed(0));
  assert!(matches!(kept.block_on(), Exit::Success(6)));

  // A defect skips the pending `map` and is not a typed failure, so `catch` passes it on.
  let died: LocalEffect<u8, ()> = LocalEffect::<u8, &str>::sync(panics)
    .map(|n| n + 1)
    .catch(|_| LocalEffect::succeed(0));
  assert!(matches!(died.block_on(), Exit::Failure(Cause::Die(_))));

  let observed: LocalEffect<Exit<u8, &str>, Never> = LocalEffect::sync(|| 7).exit();
  assert!(matches!(
    observed.block_on(),
    Exit::Success(Exit::Success(7))
  ));
}

#[test]
fn a_dying_finalizer_replaces_a_success() {
  let eff: LocalEffect<u8, &str> = LocalEffect::sync(|| 7).ensuring(LocalEffect::die("finalizer"));
  match eff.block_on() {
    Exit::Failure(Cause::Die(d)) => assert!(d.message().contains("finalizer"), "{d:?}"),
    other => panic!("expected the finalizer's defect: {other:?}"),
  }
}

#[test]
fn a_continuation_that_yields_suspends_and_resumes() {
  let eff: LocalEffect<u8> = LocalEffect::sync(|| 1)
    .flat_map(|n| LocalEffect::<(), Never>::yield_fiber().map(move |()| n + 1))
    .flat_map(|n| LocalEffect::<(), Never>::yield_fiber().map(move |()| n * 10));
  assert!(matches!(eff.block_on(), Exit::Success(20)));
}

#[test]
fn debug_names_the_node_and_the_run_state() {
  assert_eq!(
    format!("{:?}", LocalEffect::<u8>::succeed(1)),
    "LocalEffect::Ready"
  );
  assert_eq!(
    format!("{:?}", LocalEffect::<u8>::sync(|| 1)),
    "LocalEffect::Op"
  );

  let exec: Arc<dyn Executor> = Arc::new(TestExecutor::new());
  let ready = LocalEffect::<u8>::succeed(3).run_on(FiberContext::new_root(Arc::clone(&exec)));
  assert_eq!(format!("{ready:?}"), "LocalRunFuture::Ready");
  assert!(matches!(take_ready(ready), Exit::Success(3)));

  let running = LocalEffect::<u8>::sync(|| 4).run_on(FiberContext::new_root(exec));
  assert_eq!(format!("{running:?}"), "LocalRunFuture::Running");
  assert!(matches!(take_ready(running), Exit::Success(4)));
}
