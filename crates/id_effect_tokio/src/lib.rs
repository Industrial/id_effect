//! Tokio integration for [`id_effect`], on the 0.7 kernel.
//!
//! The executor lives in core: [`TokioExecutor`] (re-exported here) runs every fiber as an
//! ordinary tokio task, with real wakers and tokio timers, and [`Runtime`] owns the run edges.
//! This crate is the thin tokio-facing layer on top of them:
//!
//! - [`TokioRuntime`] pairs a [`Runtime`] with the tokio runtime it runs on. It either owns one
//!   ([`TokioRuntime::new_current_thread`], [`TokioRuntime::new_multi_thread`]) or wraps an
//!   ambient one ([`TokioRuntime::current`], [`TokioRuntime::from_handle`], e.g. under
//!   `#[tokio::main]` or `axum::serve`).
//! - [`TokioRuntime::run`] is the async edge, [`TokioRuntime::fork`] starts a daemon root fiber,
//!   and [`TokioRuntime::spawn`] runs an effect as a tokio task joined by a
//!   [`tokio::task::JoinHandle`].
//! - [`TokioRuntime::block_on`] is a program edge (`main`, tests, benches) on an owned runtime.
//!
//! Fibers are [`Send`], so they run on tokio's worker threads like any other task, and a sleeping
//! fiber is parked on the tokio timer. (0.6 drove each forked effect with `run_blocking` on
//! tokio's blocking pool, busy-polling one OS thread per fiber; that driver and
//! `spawn_blocking_run_async` are gone.) Sleep and yield are the kernel's own
//! [`id_effect::sleep`] and [`id_effect::yield_fiber`]: they use the fiber's
//! executor, so they need no runtime argument.
//!
//! ```
//! use std::time::Duration;
//!
//! use id_effect::Exit;
//! use id_effect::{HNil, sleep};
//! use id_effect_tokio::TokioRuntime;
//!
//! let rt = TokioRuntime::new_current_thread().expect("tokio runtime");
//! let exit = rt.block_on(async {
//!   let nap = sleep::<(), HNil>(Duration::from_millis(1)).map(|()| 42_u8);
//!   rt.fork(nap).join_future().await
//! });
//! assert!(matches!(exit, Exit::Success(42)));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use id_effect::Exit;
use id_effect::{Effect, Executor, FiberHandle, HNil, Runtime};

pub use id_effect::TokioExecutor;

/// An owned tokio runtime, shut down without blocking when the last [`TokioRuntime`] clone drops
/// (dropping a tokio runtime inside an async context panics, and its default shutdown waits on
/// tasks).
struct OwnedTokio(Option<tokio::runtime::Runtime>);

impl Drop for OwnedTokio {
  fn drop(&mut self) {
    if let Some(rt) = self.0.take() {
      rt.shutdown_background();
    }
  }
}

/// A [`Runtime`] on a [`TokioExecutor`], plus the tokio runtime it runs on (owned or ambient).
///
/// Cloning is cheap; clones share the executor and, when owned, the tokio runtime.
#[derive(Clone)]
pub struct TokioRuntime {
  runtime: Runtime,
  executor: TokioExecutor,
  owned: Option<Arc<OwnedTokio>>,
}

impl fmt::Debug for TokioRuntime {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TokioRuntime")
      .field("executor", &self.executor)
      .field("owns_tokio_runtime", &self.owned.is_some())
      .finish()
  }
}

impl TokioRuntime {
  fn with_parts(handle: tokio::runtime::Handle, owned: Option<tokio::runtime::Runtime>) -> Self {
    let executor = TokioExecutor::new(handle);
    Self {
      runtime: Runtime::from_executor(Arc::new(executor.clone())),
      executor,
      owned: owned.map(|rt| Arc::new(OwnedTokio(Some(rt)))),
    }
  }

  fn owning(rt: tokio::runtime::Runtime) -> Self {
    let handle = rt.handle().clone();
    Self::with_parts(handle, Some(rt))
  }

  /// A runtime on the tokio runtime the calling thread is in.
  ///
  /// # Errors
  ///
  /// Outside a tokio runtime context.
  pub fn current() -> Result<Self, std::io::Error> {
    let handle = tokio::runtime::Handle::try_current()
      .map_err(|e| std::io::Error::other(format!("no current tokio runtime: {e}")))?;
    Ok(Self::from_handle(handle))
  }

  /// A runtime on the tokio runtime behind `handle` (e.g. from `#[tokio::main]` or
  /// `axum::serve`). It owns nothing; that runtime must have its time driver enabled.
  #[inline]
  pub fn from_handle(handle: tokio::runtime::Handle) -> Self {
    Self::with_parts(handle, None)
  }

  /// Owns a new current-thread tokio runtime (all drivers enabled). Fibers make progress while
  /// [`Self::block_on`] drives it.
  ///
  /// # Errors
  ///
  /// If the tokio runtime cannot be built.
  pub fn new_current_thread() -> Result<Self, std::io::Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()?;
    Ok(Self::owning(rt))
  }

  /// Owns a new multi-thread tokio runtime (all drivers enabled), typical for CLIs and servers
  /// without `#[tokio::main]`.
  ///
  /// # Errors
  ///
  /// If the tokio runtime cannot be built (for example, worker threads cannot be spawned).
  pub fn new_multi_thread() -> Result<Self, std::io::Error> {
    let rt = tokio::runtime::Builder::new_multi_thread()
      .enable_all()
      .build()?;
    Ok(Self::owning(rt))
  }

  /// The runtime (run edges) over this adapter's executor.
  #[inline]
  pub fn runtime(&self) -> &Runtime {
    &self.runtime
  }

  /// The executor fibers run on.
  #[inline]
  pub fn executor(&self) -> &TokioExecutor {
    &self.executor
  }

  /// The tokio handle fibers are spawned onto.
  #[inline]
  pub fn handle(&self) -> tokio::runtime::Handle {
    self.executor.handle().clone()
  }

  /// The async edge ([`Runtime::run`]): runs `eff` as a root fiber, driven by whoever polls the
  /// returned future. Its children run on this adapter's tokio runtime.
  pub fn run<A, E>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> impl Future<Output = Exit<A, E>> + Send + 'static
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    self.runtime.run(eff)
  }

  /// Starts `eff` as a daemon root fiber ([`Runtime::fork`]); join it with
  /// [`FiberHandle::join_future`] or the `join` effect, or interrupt it.
  pub fn fork<A, E>(&self, eff: Effect<A, E, HNil>) -> FiberHandle<A, E>
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    self.runtime.fork(eff)
  }

  /// Runs `eff` as a root fiber inside a new tokio task (the successor of 0.6's
  /// `spawn_blocking_run_async`); the effect no longer has to be driven on the blocking pool.
  pub fn spawn<A, E>(&self, eff: Effect<A, E, HNil>) -> tokio::task::JoinHandle<Exit<A, E>>
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    self.executor.handle().spawn(self.runtime.run(eff))
  }

  /// The executor's clock (tokio's clock: the wall clock unless the tokio clock is paused).
  #[inline]
  pub fn now(&self) -> Instant {
    self.executor.now()
  }

  /// Program edge: drives `f` to completion on the owned tokio runtime (built with
  /// [`Self::new_current_thread`] or [`Self::new_multi_thread`]), blocking the calling thread.
  /// Use it in `main`, tests and benches only; library code `.await`s [`Self::run`].
  ///
  /// # Panics
  ///
  /// When this adapter does not own its tokio runtime ([`Self::from_handle`],
  /// [`Self::current`]): use the surrounding runtime's `block_on` or `.await` instead. Tokio
  /// also panics when this is called inside an async context.
  pub fn block_on<F: Future>(&self, f: F) -> F::Output {
    match self.owned.as_ref().and_then(|o| o.0.as_ref()) {
      Some(rt) => rt.block_on(f),
      None => panic!(
        "TokioRuntime::block_on requires TokioRuntime::new_current_thread() or new_multi_thread(); \
         otherwise `.await` TokioRuntime::run under the surrounding runtime"
      ),
    }
  }
}

impl AsRef<Runtime> for TokioRuntime {
  #[inline]
  fn as_ref(&self) -> &Runtime {
    &self.runtime
  }
}

impl From<TokioRuntime> for Runtime {
  /// The runtime; an owned tokio runtime is kept alive by the remaining
  /// [`TokioRuntime`] clones only.
  #[inline]
  fn from(rt: TokioRuntime) -> Self {
    rt.runtime
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use id_effect::Cause;
  use id_effect::{fail, sleep, succeed, sync, yield_fiber};

  use super::*;

  fn nap(ms: u64) -> Effect<(), (), HNil> {
    sleep(Duration::from_millis(ms))
  }

  #[test]
  fn new_current_thread_runs_sleep_and_yield_under_block_on() {
    let rt = TokioRuntime::new_current_thread().expect("tokio runtime should build");
    let exit = rt.block_on(rt.run(nap(0).then(yield_fiber()).map(|()| 42_u8)));
    assert!(matches!(exit, Exit::Success(42)));
  }

  #[test]
  fn fork_runs_effect_to_completion_and_join_returns_its_exit() {
    let rt = TokioRuntime::new_current_thread().expect("tokio runtime should build");
    let ok = rt.block_on(rt.fork(succeed::<u8, (), HNil>(7)).join_future());
    assert!(matches!(ok, Exit::Success(7)));
    let err = rt.block_on(rt.fork(fail::<u8, &str, HNil>("boom")).join_future());
    assert!(matches!(err, Exit::Failure(Cause::Fail("boom"))));
  }

  #[test]
  fn spawn_returns_a_tokio_join_handle_with_the_exit() {
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime should build");
    let exit = rt.block_on(async {
      rt.spawn(nap(1).map(|()| 9_u8))
        .await
        .expect("task should not panic")
    });
    assert!(matches!(exit, Exit::Success(9)));
  }

  /// 64 concurrently sleeping fibers share the 2 tokio workers: 0.6 parked (spun) one
  /// blocking-pool thread per sleeping fiber.
  #[test]
  fn sleeping_fibers_share_the_tokio_workers_instead_of_one_thread_each() {
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(2)
      .enable_time()
      .build()
      .expect("tokio runtime should build");
    let rt = TokioRuntime::from_handle(tokio_rt.handle().clone());
    let threads = tokio_rt.block_on(async {
      let handles: Vec<_> = (0..64)
        .map(|_| rt.fork(nap(20).then(sync::<_, (), HNil>(|| std::thread::current().id()))))
        .collect();
      let mut threads = std::collections::HashSet::new();
      for h in handles {
        match h.join_future().await {
          Exit::Success(id) => threads.insert(id),
          Exit::Failure(c) => panic!("fiber failed: {c:?}"),
        };
      }
      threads
    });
    assert!(
      threads.len() <= 2,
      "fibers ran on {} threads",
      threads.len()
    );
  }

  #[test]
  fn interrupting_a_sleeping_fiber_ends_it_with_interrupt() {
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime should build");
    let exit = rt.block_on(async {
      let h = rt.fork(nap(60_000));
      h.interrupt_now();
      h.join_future().await
    });
    assert!(matches!(exit, Exit::Failure(Cause::Interrupt(_))));
  }

  #[tokio::test]
  async fn from_handle_runs_under_the_current_context() {
    let rt = TokioRuntime::from_handle(tokio::runtime::Handle::current());
    assert!(!format!("{rt:?}").contains("owns_tokio_runtime: true"));
    assert!(matches!(rt.run(nap(0)).await, Exit::Success(())));
    assert!(matches!(
      rt.fork(nap(0)).join_future().await,
      Exit::Success(())
    ));
  }

  #[tokio::test]
  async fn current_succeeds_inside_tokio_context() {
    let rt = TokioRuntime::current().expect("current should work inside #[tokio::test]");
    assert!(matches!(
      rt.run(yield_fiber()).await,
      Exit::<(), ()>::Success(())
    ));
  }

  #[tokio::test]
  async fn dropping_an_owned_runtime_inside_async_context_does_not_panic() {
    let owned = TokioRuntime::new_current_thread().expect("runtime");
    drop(owned.clone());
    drop(owned);
  }

  #[test]
  fn runtime_accessors_share_the_executor() {
    let rt = TokioRuntime::new_current_thread().expect("runtime");
    let next_rt: &Runtime = rt.as_ref();
    assert!(std::ptr::eq(next_rt, rt.runtime()));
    let handle = rt.handle();
    assert_eq!(rt.executor().handle().id(), handle.id());
    let owned: Runtime = rt.clone().into();
    assert!(matches!(
      rt.block_on(owned.run(succeed::<u8, (), HNil>(3))),
      Exit::Success(3)
    ));
  }

  #[test]
  fn now_returns_monotonic_instant() {
    let rt = TokioRuntime::new_current_thread().expect("runtime");
    let t1 = rt.now();
    let t2 = rt.now();
    assert!(t2 >= t1, "now() should be non-decreasing");
  }

  #[test]
  fn current_fails_when_no_tokio_runtime() {
    let res = std::thread::spawn(TokioRuntime::current)
      .join()
      .expect("thread should not panic");
    let err = match res {
      Err(e) => e,
      Ok(_) => panic!("expected Err outside a Tokio context"),
    };
    assert!(
      err.to_string().contains("no current tokio runtime"),
      "unexpected error: {err}"
    );
  }

  #[test]
  #[should_panic(expected = "TokioRuntime::block_on requires")]
  fn block_on_panics_when_adapter_has_no_owned_runtime() {
    let owned = TokioRuntime::new_current_thread().expect("runtime");
    let adapter = TokioRuntime::from_handle(owned.handle());
    adapter.block_on(async {});
  }
}
