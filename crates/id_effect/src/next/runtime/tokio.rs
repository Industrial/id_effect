//! `TokioExecutor`: the default `Executor` backend, on a tokio runtime handle.
//!
//! Inside this module the tokio crate is named `::tokio`, because the module itself is called
//! `tokio`.

use std::fmt;
use std::time::{Duration, Instant};

use super::executor::{BoxFuture, Executor, LocalBoxFuture, NotLocal};

/// The default [`Executor`]: spawns onto a tokio runtime through its [`Handle`].
///
/// - `spawn` uses [`Handle::spawn`] and `spawn_blocking` uses [`Handle::spawn_blocking`] (tokio's
///   blocking pool).
/// - `sleep` is a real tokio timer ([`::tokio::time::sleep`]); the runtime must have its time
///   driver enabled.
/// - `now` reads tokio's clock ([`::tokio::time::Instant`]), which is the wall clock unless the
///   runtime's clock is paused.
/// - `spawn_local` always returns `Err(NotLocal)`: there is no `LocalSet` integration yet, so
///   `!Send` work runs on the calling thread at the run edge instead (an input for
///   `c-compat-bridge`).
///
/// [`Handle`]: ::tokio::runtime::Handle
/// [`Handle::spawn`]: ::tokio::runtime::Handle::spawn
/// [`Handle::spawn_blocking`]: ::tokio::runtime::Handle::spawn_blocking
#[derive(Clone)]
pub struct TokioExecutor {
  handle: ::tokio::runtime::Handle,
}

impl TokioExecutor {
  /// An executor that spawns onto the runtime behind `handle`.
  pub fn new(handle: ::tokio::runtime::Handle) -> Self {
    Self { handle }
  }

  /// The executor of the tokio runtime the calling thread is in, or `None` outside one.
  pub fn current() -> Option<Self> {
    ::tokio::runtime::Handle::try_current().ok().map(Self::new)
  }

  /// The tokio runtime handle this executor spawns onto.
  pub fn handle(&self) -> &::tokio::runtime::Handle {
    &self.handle
  }
}

impl fmt::Debug for TokioExecutor {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TokioExecutor")
      .field("runtime_flavor", &self.handle.runtime_flavor())
      .finish()
  }
}

impl Executor for TokioExecutor {
  fn spawn(&self, fut: BoxFuture<'static, ()>) {
    drop(self.handle.spawn(fut));
  }

  /// Always `Err(NotLocal)`: no `LocalSet` integration yet.
  fn spawn_local(&self, _fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }

  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>) {
    drop(self.handle.spawn_blocking(f));
  }

  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    // `tokio::time::sleep` registers with the timer of the runtime it is created in.
    let _guard = self.handle.enter();
    Box::pin(::tokio::time::sleep(d))
  }

  fn now(&self) -> Instant {
    let _guard = self.handle.enter();
    ::tokio::time::Instant::now().into_std()
  }
}
