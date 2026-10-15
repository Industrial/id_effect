//! `k-core-runloop` AC1 for the 0.7 kernel: inline `Pure` allocates nothing.
//!
//! This is the **only** test in this binary on purpose. `stats_alloc` counts allocations
//! process-wide, and the package forbids `unsafe_code` in every target, so a thread-scoped
//! `GlobalAlloc` cannot be written here. With a single test there is no other test thread to
//! allocate inside the measured region, under plain `cargo test` as well as under nextest
//! (which runs each test in its own process). Do not add tests to this file.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use id_effect::Exit;
use id_effect::{BoxFuture, Effect, Executor, FiberContext, LocalBoxFuture, NotLocal, succeed};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<std::alloc::System> = &INSTRUMENTED_SYSTEM;

/// Spawns nothing; the pure fast path never reaches the executor.
struct StubExec;

impl Executor for StubExec {
  fn spawn(&self, _: BoxFuture<'static, ()>) {
    unreachable!("the pure fast path spawns no tasks")
  }
  fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }
  fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
    unreachable!("the pure fast path spawns no blocking work")
  }
  fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
    unreachable!("the pure fast path never sleeps")
  }
  fn now(&self) -> Instant {
    Instant::now()
  }
}

#[test]
fn succeed_and_drive_through_the_pure_fast_path_allocates_nothing() {
  let fiber = FiberContext::new_root(Arc::new(StubExec));
  let waker = Waker::noop();
  let mut cx = Context::from_waker(waker);
  let region = Region::new(GLOBAL);
  let eff: Effect<u32> = succeed(1);
  let mut fut = eff.run_on(Arc::clone(&fiber));
  let out = Pin::new(&mut fut).poll(&mut cx);
  let stats = region.change();
  assert!(matches!(out, Poll::Ready(Exit::Success(1))));
  assert_eq!(stats.allocations, 0, "{stats:?}");
  assert_eq!(stats.reallocations, 0, "{stats:?}");
}
