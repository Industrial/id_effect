//! `Runtime` (an executor, optionally owning its tokio runtime) and its run edges: `run` (async),
//! `try_block_on` / `block_on` (park the calling thread on a real waker), `block_on_future`, and
//! `BlockOnInAsync`. Filled by `k-edge`.
//!
//! # The blocking edge
//!
//! The root fiber of a blocking run is driven **on the calling thread**: the edge polls the
//! effect's runloop with a [`Waker`] built from [`std::task::Wake`] over [`Thread::unpark`], and
//! parks the thread between wake-ups. A sleeping or suspended effect costs no CPU (no busy poll
//! and no yield spin; v1's `runtime/execute.rs:62-79` is what this replaces).
//!
//! Parking an executor thread can deadlock (the `request_resolver.rs:102` bug class), so the
//! blocking edges refuse to run inside an async context and return [`BlockOnInAsync`] instead.
//! "Async context" is detected conservatively: inside a tokio task **or** anywhere a tokio
//! runtime context is entered (`#[tokio::main]`/`#[tokio::test]` bodies, `Runtime::enter` guards,
//! and tokio's blocking-pool threads). From those places, `.await` [`Runtime::run`] instead (on a
//! blocking-pool thread: `Handle::block_on(rt.run(eff))`).
//!
//! # Children
//!
//! The edges run the root on a fresh root [`FiberContext`] through
//! [`Effect::run_fiber`](crate::next::Effect::run_fiber): when the root's own program finishes,
//! its live children are interrupted and awaited before the exit is returned (structured
//! concurrency). [`Runtime::fork`] starts a daemon root fiber and returns its handle.

use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use crate::failure::{Cause, Defect, Exit};
use crate::next::capability::HNil;
use crate::next::kernel::{
  Effect, Node, cause_from_panic, hnil_env, settle_instr, to_exit, unerase_exit,
};
use crate::next::runtime::{Executor, FiberContext, FiberHandle, TokioExecutor};

/// A blocking edge (`try_block_on`, `block_on_future`) was called inside an async context, where
/// parking the thread could block an executor thread and deadlock. Use `Runtime::run(..).await`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockOnInAsync;

impl fmt::Display for BlockOnInAsync {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(
      "next: block_on called inside an async context (tokio task or runtime context); \
       use `Runtime::run(..).await` instead",
    )
  }
}

impl std::error::Error for BlockOnInAsync {}

/// Wakes a parked thread.
struct Park(Thread);

impl Wake for Park {
  fn wake(self: Arc<Self>) {
    self.0.unpark();
  }

  fn wake_by_ref(self: &Arc<Self>) {
    self.0.unpark();
  }
}

/// `true` inside a tokio task or an entered tokio runtime context (see the module docs).
pub(crate) fn in_async_context() -> bool {
  ::tokio::task::try_id().is_some() || ::tokio::runtime::Handle::try_current().is_ok()
}

/// Polls `fut` on the calling thread, parking between wake-ups (a real waker, no spin).
///
/// A spurious unpark only causes one extra poll. Callers check [`in_async_context`] first.
pub(crate) fn park_on<F: Future>(fut: F) -> F::Output {
  let waker = Waker::from(Arc::new(Park(thread::current())));
  let mut cx = Context::from_waker(&waker);
  let mut fut = std::pin::pin!(fut);
  loop {
    if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
      return v;
    }
    thread::park();
  }
}

/// Settles the fiber-free prefix of an effect without a fiber, runloop or executor (`Pure`, `Sync`
/// closures, and `map`/`flat_map`/`Fold` frames over them: see `settle_instr`); returns the rest
/// of the program once it reaches a step that needs a fiber.
///
/// A panic in a step becomes `Die` (as in the runloop).
fn settle<A, E>(eff: Effect<A, E, HNil>) -> Result<Exit<A, E>, Effect<A, E, HNil>>
where
  A: Send + 'static,
  E: Send + 'static,
{
  let mut node = match eff.node {
    Node::Pure(r) => return Ok(to_exit(r)),
    node => node,
  };
  let out = catch_unwind(AssertUnwindSafe(move || {
    loop {
      match node {
        Node::Pure(r) => return Ok(r),
        Node::Sync(f, _) => node = f(&HNil).node,
        Node::Op(i) => return Err(i),
      }
    }
  }));
  let instr = match out {
    Ok(Ok(r)) => return Ok(to_exit(r)),
    Ok(Err(i)) => i,
    Err(payload) => return Ok(Exit::Failure(cause_from_panic(payload))),
  };
  match settle_instr(instr, hnil_env()) {
    Ok(exit) => Ok(to_exit(unerase_exit::<A, E>(exit))),
    Err(rest) => Err(Effect::from_instr(rest)),
  }
}

/// Runs `eff` as a root fiber on `exec`, on the polling task (no task is spawned).
fn drive_root<A, E>(
  exec: Arc<dyn Executor>,
  eff: Effect<A, E, HNil>,
) -> impl Future<Output = Exit<A, E>> + Send + 'static
where
  A: Send + 'static,
  E: Send + 'static,
{
  // `run_fiber` interrupts and awaits the root's live children before its exit is returned.
  eff.run_fiber(FiberContext::new_root(exec))
}

/// Drops the owned tokio runtime without blocking (dropping a tokio runtime inside an async
/// context panics, and its default shutdown waits on tasks).
struct OwnedTokio(Option<::tokio::runtime::Runtime>);

impl Drop for OwnedTokio {
  fn drop(&mut self) {
    if let Some(rt) = self.0.take() {
      rt.shutdown_background();
    }
  }
}

/// An [`Executor`] plus the run edges. Cloning is cheap; clones share the executor and, for
/// [`Runtime::new`], the owned tokio runtime (shut down without blocking when the last clone
/// drops).
#[derive(Clone)]
pub struct Runtime {
  exec: Arc<dyn Executor>,
  owned: Option<Arc<OwnedTokio>>,
}

impl fmt::Debug for Runtime {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Runtime")
      .field("owns_tokio_runtime", &self.owned.is_some())
      .finish_non_exhaustive()
  }
}

impl Runtime {
  /// A runtime that owns a new multi-thread tokio runtime (time driver enabled) and runs fibers
  /// on its [`TokioExecutor`].
  ///
  /// # Errors
  ///
  /// If the tokio runtime cannot be built (for example, worker threads cannot be spawned).
  pub fn new() -> std::io::Result<Self> {
    let rt = ::tokio::runtime::Builder::new_multi_thread()
      .enable_time()
      .thread_name("id-effect-next")
      .build()?;
    Ok(Self {
      exec: Arc::new(TokioExecutor::new(rt.handle().clone())),
      owned: Some(Arc::new(OwnedTokio(Some(rt)))),
    })
  }

  /// A runtime over an existing executor (a [`TokioExecutor`] on an ambient runtime, a
  /// [`TestExecutor`](crate::next::TestExecutor), …). It owns nothing.
  ///
  /// With a `TestExecutor`, the blocking edges only finish effects that need no executor work
  /// (nothing drives the executor while the thread is parked); drive it with
  /// `run_until_stalled`/`advance` and poll [`run`](Self::run) instead.
  pub fn from_executor(exec: Arc<dyn Executor>) -> Self {
    Self { exec, owned: None }
  }

  /// The process-wide default runtime (created on first use, never dropped). This is the
  /// runtime behind [`Effect::block_on`](crate::next::Effect::block_on).
  ///
  /// # Panics
  ///
  /// On first use, if the default tokio runtime cannot be started.
  pub fn global() -> &'static Runtime {
    static GLOBAL: OnceLock<Runtime> = OnceLock::new();
    GLOBAL.get_or_init(|| Runtime::new().expect("next: cannot start the default tokio runtime"))
  }

  /// The executor fibers run on.
  pub fn executor(&self) -> &Arc<dyn Executor> {
    &self.exec
  }

  /// The async edge: runs `eff` as a root fiber, driven by whoever polls the returned future
  /// (no task is spawned). The future is `Send + 'static`, so it can also be spawned.
  ///
  /// A `Pure` effect completes without creating a fiber or touching the executor.
  pub fn run<A, E>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> impl Future<Output = Exit<A, E>> + Send + 'static
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    let exec = Arc::clone(&self.exec);
    async move {
      match eff.node {
        Node::Pure(r) => to_exit(r),
        node => drive_root(exec, Effect { node }).await,
      }
    }
  }

  /// The blocking edge: runs `eff` as a root fiber on the calling thread, parking it between
  /// wake-ups (see the module docs).
  ///
  /// Fused synchronous effects (`succeed`, `sync`, `map`/`flat_map` chains over them) settle
  /// inline, without a fiber, a runloop or the executor.
  ///
  /// # Errors
  ///
  /// [`BlockOnInAsync`] when called inside an async context; nothing is run.
  pub fn try_block_on<A, E>(&self, eff: Effect<A, E, HNil>) -> Result<Exit<A, E>, BlockOnInAsync>
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    if in_async_context() {
      return Err(BlockOnInAsync);
    }
    Ok(match settle(eff) {
      Ok(exit) => exit,
      Err(eff) => park_on(drive_root(Arc::clone(&self.exec), eff)),
    })
  }

  /// [`try_block_on`](Self::try_block_on), with [`BlockOnInAsync`] reported as a defect
  /// (`Exit::Failure(Cause::Die(..))`) instead of a separate error.
  #[allow(
    clippy::disallowed_methods,
    reason = "G-nb: the blocking edge itself, delegating to `try_block_on`"
  )]
  pub fn block_on<A, E>(&self, eff: Effect<A, E, HNil>) -> Exit<A, E>
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    self
      .try_block_on(eff)
      .unwrap_or_else(|e| Exit::Failure(Cause::Die(Defect::new(e.to_string()))))
  }

  /// Spawns `eff` as a daemon root fiber (no parent) on this runtime's executor and returns
  /// its handle; join it with `handle.join_future()` (async) or the `join` effect.
  pub fn fork<A, E>(&self, eff: Effect<A, E, HNil>) -> FiberHandle<A, E>
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    eff.fork_root(Arc::clone(&self.exec))
  }

  /// Blocks the calling thread on an arbitrary future with the parking waker (test, bench and
  /// bridge edge). Executor-independent: the future must be woken by whatever it waits on.
  ///
  /// # Errors
  ///
  /// [`BlockOnInAsync`] when called inside an async context; `fut` is dropped unpolled.
  pub fn block_on_future<F: Future>(&self, fut: F) -> Result<F::Output, BlockOnInAsync> {
    if in_async_context() {
      return Err(BlockOnInAsync);
    }
    Ok(park_on(fut))
  }
}
