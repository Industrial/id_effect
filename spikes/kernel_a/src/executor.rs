//! The `Executor` trait, its tokio backend, and the run edges (design A §1.3): `Runtime::run`
//! (async), `Runtime::block_on` (parks a real thread waker; refuses inside a tokio task).

use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use crate::cause::{Cause, Defect, Exit, cause_from_panic};
use crate::effect::{Effect, Node, unerase_exit};
use crate::fiber::{FiberHandle, run_root, spawn_fiber};
use crate::hlist::HNil;
use crate::runloop::{AnyEnv, BoxFuture, LocalBoxFuture};

/// `spawn_local` outside a local set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotLocal;

/// Where fibers run. Tokio is the default (and in the spike, the only) backend.
pub trait Executor: Send + Sync + 'static {
  fn spawn(&self, fut: BoxFuture<'static, ()>);
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal>;
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>);
  /// A real timer (no `thread::sleep`, no spinning).
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;
  fn now(&self) -> Instant;
}

/// The tokio backend.
pub struct TokioExecutor {
  handle: tokio::runtime::Handle,
}

impl TokioExecutor {
  pub fn new(handle: tokio::runtime::Handle) -> Self {
    Self { handle }
  }
}

impl Executor for TokioExecutor {
  fn spawn(&self, fut: BoxFuture<'static, ()>) {
    drop(self.handle.spawn(fut));
  }

  /// Spike limitation: no `LocalSet` integration, so local fibers cannot be spawned; a
  /// `LocalEffect` runs on the calling thread via `LocalEffect::block_on`.
  fn spawn_local(&self, _fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
    Err(NotLocal)
  }

  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>) {
    drop(self.handle.spawn_blocking(f));
  }

  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()> {
    let _guard = self.handle.enter();
    Box::pin(tokio::time::sleep(d))
  }

  fn now(&self) -> Instant {
    Instant::now()
  }
}

/// `block_on` was called from inside an async task, where parking would block an executor
/// thread (the `request_resolver.rs:102` bug class).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockOnInAsync;

impl fmt::Display for BlockOnInAsync {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("block_on called inside an async task; use `Runtime::run(..).await` instead")
  }
}

impl std::error::Error for BlockOnInAsync {}

struct Park(Thread);

impl Wake for Park {
  fn wake(self: Arc<Self>) {
    self.0.unpark();
  }
  fn wake_by_ref(self: &Arc<Self>) {
    self.0.unpark();
  }
}

thread_local! {
  static PARK_WAKER: Waker = Waker::from(Arc::new(Park(thread::current())));
}

/// Poll `fut` on this thread, parking between wake-ups (a real waker, no spin).
fn park_on<F: Future>(fut: F) -> F::Output {
  let waker = PARK_WAKER.with(Waker::clone);
  let mut cx = Context::from_waker(&waker);
  let mut fut = std::pin::pin!(fut);
  loop {
    if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
      return v;
    }
    thread::park();
  }
}

fn in_async_task() -> bool {
  tokio::task::try_id().is_some()
}

/// A handle to an executor plus the run edges.
#[derive(Clone)]
pub struct Runtime {
  exec: Arc<dyn Executor>,
  _owned: Option<Arc<tokio::runtime::Runtime>>,
}

impl fmt::Debug for Runtime {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Runtime").finish_non_exhaustive()
  }
}

fn empty_env() -> AnyEnv {
  static EMPTY: OnceLock<AnyEnv> = OnceLock::new();
  Arc::clone(EMPTY.get_or_init(|| Arc::new(HNil)))
}

impl Runtime {
  /// A runtime owning a multi-thread tokio runtime.
  pub fn new() -> std::io::Result<Self> {
    let rt = tokio::runtime::Builder::new_multi_thread()
      .enable_time()
      .build()?;
    Ok(Self {
      exec: Arc::new(TokioExecutor::new(rt.handle().clone())),
      _owned: Some(Arc::new(rt)),
    })
  }

  pub fn from_executor(exec: Arc<dyn Executor>) -> Self {
    Self { exec, _owned: None }
  }

  /// The process-wide default runtime used by `Effect::block_on`.
  pub fn global() -> &'static Runtime {
    static GLOBAL: OnceLock<Runtime> = OnceLock::new();
    GLOBAL.get_or_init(|| Runtime::new().expect("kernel_a: cannot start the default tokio runtime"))
  }

  pub fn executor(&self) -> &Arc<dyn Executor> {
    &self.exec
  }

  /// The async edge: run `eff` as a root fiber on the calling task.
  pub async fn run<A: Send + 'static, E: Send + 'static>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> Exit<A, E> {
    match eff.node {
      Node::Pure(r) => r,
      node => {
        let instr = Effect { node }.into_instr();
        unerase_exit(run_root(instr, empty_env(), Arc::clone(&self.exec)).await)
      }
    }
  }

  /// The blocking edge. Inside a tokio task this returns `Err(BlockOnInAsync)` instead of
  /// blocking an executor thread.
  pub fn try_block_on<A: Send + 'static, E: Send + 'static>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> Result<Exit<A, E>, BlockOnInAsync> {
    if in_async_task() {
      return Err(BlockOnInAsync);
    }
    // Fast path: fused synchronous effects never build a fiber or a runloop.
    let mut node = eff.node;
    let fast = catch_unwind(AssertUnwindSafe(move || {
      loop {
        match node {
          Node::Pure(r) => return Ok(r),
          Node::Sync(f, _) => node = f(&HNil).node,
          node @ Node::Op(_) => return Err(Effect { node }.into_instr()),
        }
      }
    }));
    match fast {
      Ok(Ok(exit)) => Ok(exit),
      Ok(Err(instr)) => Ok(unerase_exit(park_on(run_root(
        instr,
        empty_env(),
        Arc::clone(&self.exec),
      )))),
      Err(p) => Ok(Err(cause_from_panic(p))),
    }
  }

  /// [`Self::try_block_on`], with `BlockOnInAsync` reported as a defect.
  pub fn block_on<A: Send + 'static, E: Send + 'static>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> Exit<A, E> {
    self
      .try_block_on(eff)
      .unwrap_or_else(|e| Err(Cause::Die(Defect::new(e.to_string()))))
  }

  /// Block on an arbitrary future with the parking waker (bench/test edge).
  pub fn block_on_future<F: Future>(&self, fut: F) -> Result<F::Output, BlockOnInAsync> {
    if in_async_task() {
      return Err(BlockOnInAsync);
    }
    Ok(park_on(fut))
  }

  /// Fork `eff` as a daemon fiber (no parent; the 0.6 `run_fork` analogue).
  pub fn fork<A: Send + 'static, E: Send + 'static>(
    &self,
    eff: Effect<A, E, HNil>,
  ) -> FiberHandle<A, E> {
    let (ctx, cell) = spawn_fiber(eff.into_instr(), empty_env(), None, &self.exec);
    FiberHandle::new(ctx, cell)
  }
}
