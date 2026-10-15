//! The `Executor` trait: where fibers run (ADR 0011). Tokio (`TokioExecutor`) is the default
//! backend; `TestExecutor` is the deterministic one for tests.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

/// A boxed, `Send` future, as handed to [`Executor::spawn`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed future that need not be `Send`, as handed to [`Executor::spawn_local`].
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Returned by [`Executor::spawn_local`] when the executor cannot run `!Send` futures on the
/// current thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotLocal;

/// Where fibers run.
///
/// Implementations must use real wakers and real timers: no busy polling and no sleeping threads.
pub trait Executor: Send + Sync + 'static {
  /// Spawns a `Send` future to run to completion in the background.
  fn spawn(&self, fut: BoxFuture<'static, ()>);

  /// Spawns a `!Send` future on the current thread, or returns [`NotLocal`] if this executor
  /// has no local task set here.
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal>;

  /// Runs a blocking closure on a thread where blocking is allowed.
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>);

  /// Returns a future that completes after `d` on this executor's clock.
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;

  /// The current instant on this executor's clock (virtual for test executors).
  fn now(&self) -> Instant;
}
