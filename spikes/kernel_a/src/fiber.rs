//! Fibers (design A §1.3): a parent/child context tree, a one-shot exit slot (R1, no `watch`),
//! real wakers for interruption, and children interrupted-and-awaited when a fiber exits.

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Waker};

use crate::cause::Exit;
use crate::effect::{Effect, unerase_exit};
use crate::executor::Executor;
use crate::runloop::{AnyEnv, AnyExit, Instr, Runloop};

static NEXT_FIBER: AtomicU64 = AtomicU64::new(1);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fiber's identity; `parent` is `None` for roots and daemons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FiberId {
  pub id: u64,
  pub parent: Option<u64>,
}

/// Per-fiber runtime state shared by its runloop(s), its handle and its parent.
pub struct FiberContext {
  id: FiberId,
  exec: Arc<dyn Executor>,
  interrupt: AtomicBool,
  mask: AtomicU32,
  waker: Mutex<Option<Waker>>,
  children: Mutex<HashMap<u64, Arc<FiberContext>>>,
  parent: Option<Weak<FiberContext>>,
  done: AtomicBool,
  done_wakers: Mutex<Vec<Waker>>,
}

impl std::fmt::Debug for FiberContext {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FiberContext")
      .field("id", &self.id)
      .finish_non_exhaustive()
  }
}

impl FiberContext {
  pub(crate) fn new(parent: Option<&Arc<FiberContext>>, exec: Arc<dyn Executor>) -> Arc<Self> {
    Arc::new(Self {
      id: FiberId {
        id: NEXT_FIBER.fetch_add(1, Ordering::Relaxed),
        parent: parent.map(|p| p.id.id),
      },
      exec,
      interrupt: AtomicBool::new(false),
      mask: AtomicU32::new(0),
      waker: Mutex::new(None),
      children: Mutex::new(HashMap::new()),
      parent: parent.map(Arc::downgrade),
      done: AtomicBool::new(false),
      done_wakers: Mutex::new(Vec::new()),
    })
  }

  pub fn id(&self) -> FiberId {
    self.id
  }

  pub fn executor(&self) -> &Arc<dyn Executor> {
    &self.exec
  }

  pub(crate) fn mask(&self) -> u32 {
    self.mask.load(Ordering::Relaxed)
  }

  pub(crate) fn set_mask(&self, m: u32) {
    self.mask.store(m, Ordering::Relaxed);
  }

  /// An interrupt was requested and the fiber is not in an uninterruptible region.
  pub(crate) fn should_interrupt(&self) -> bool {
    self.interrupt.load(Ordering::Acquire) && self.mask() == 0
  }

  pub(crate) fn register_waker(&self, w: &Waker) {
    let mut slot = lock(&self.waker);
    match slot.as_ref() {
      Some(old) if old.will_wake(w) => {}
      _ => *slot = Some(w.clone()),
    }
  }

  /// Request interruption and wake the fiber if it is suspended (no polling, no spin).
  pub fn request_interrupt(&self) {
    self.interrupt.store(true, Ordering::Release);
    let w = lock(&self.waker).take();
    if let Some(w) = w {
      w.wake();
    }
  }

  pub fn is_done(&self) -> bool {
    self.done.load(Ordering::Acquire)
  }

  fn mark_done(&self) {
    self.done.store(true, Ordering::Release);
    let ws = std::mem::take(&mut *lock(&self.done_wakers));
    for w in ws {
      w.wake();
    }
  }

  pub(crate) fn poll_done(&self, cx: &mut Context<'_>) -> Poll<()> {
    if self.is_done() {
      return Poll::Ready(());
    }
    lock(&self.done_wakers).push(cx.waker().clone());
    if self.is_done() {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  }

  pub fn live_children(&self) -> usize {
    lock(&self.children).len()
  }
}

/// Resolves once the fiber has completed (after its own children).
pub(crate) struct Done(pub(crate) Arc<FiberContext>);

impl Future for Done {
  type Output = ();
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    self.0.poll_done(cx)
  }
}

/// Interrupt every live child of `ctx` and wait for each to finish (structured concurrency).
pub(crate) async fn finish_children(ctx: &Arc<FiberContext>) {
  loop {
    let kids: Vec<Arc<FiberContext>> = lock(&ctx.children).drain().map(|(_, c)| c).collect();
    if kids.is_empty() {
      return;
    }
    for k in &kids {
      k.request_interrupt();
    }
    for k in kids {
      Done(k).await;
    }
  }
}

/// The one-shot completion slot (R1): written once by the fiber, taken once by the joiner.
pub(crate) struct ExitCell<T> {
  state: Mutex<CellState<T>>,
}

enum CellState<T> {
  Waiting(Option<Waker>),
  Ready(T),
  Taken,
}

impl<T> ExitCell<T> {
  pub(crate) fn new() -> Self {
    Self {
      state: Mutex::new(CellState::Waiting(None)),
    }
  }

  pub(crate) fn complete(&self, v: T) {
    let prev = std::mem::replace(&mut *lock(&self.state), CellState::Ready(v));
    if let CellState::Waiting(Some(w)) = prev {
      w.wake();
    }
  }

  pub(crate) fn poll_take(&self, cx: &mut Context<'_>) -> Poll<T> {
    let mut s = lock(&self.state);
    match std::mem::replace(&mut *s, CellState::Taken) {
      CellState::Ready(v) => Poll::Ready(v),
      CellState::Waiting(_) => {
        *s = CellState::Waiting(Some(cx.waker().clone()));
        Poll::Pending
      }
      CellState::Taken => unreachable!("kernel_a: fiber exit taken twice"),
    }
  }
}

pub(crate) type Cell = Arc<ExitCell<AnyExit>>;

/// Spawn `instr` as a fiber on `exec`. With a `parent`, the child is registered in the parent's
/// tree (interrupted and awaited when the parent exits); without one it is a daemon.
pub(crate) fn spawn_fiber(
  instr: Instr,
  env: AnyEnv,
  parent: Option<&Arc<FiberContext>>,
  exec: &Arc<dyn Executor>,
) -> (Arc<FiberContext>, Cell) {
  let ctx = FiberContext::new(parent, Arc::clone(exec));
  if let Some(p) = parent {
    lock(&p.children).insert(ctx.id.id, Arc::clone(&ctx));
  }
  let cell: Cell = Arc::new(ExitCell::new());
  let task = {
    let ctx = Arc::clone(&ctx);
    let cell = Arc::clone(&cell);
    async move {
      let exit = Runloop::new(instr, env, Arc::clone(&ctx)).await;
      finish_children(&ctx).await;
      cell.complete(exit);
      ctx.mark_done();
      if let Some(p) = ctx.parent.as_ref().and_then(Weak::upgrade) {
        lock(&p.children).remove(&ctx.id.id);
      }
    }
  };
  exec.spawn(Box::pin(task));
  (ctx, cell)
}

/// Run `instr` to completion on the *current* task as a root fiber (the async edge and
/// `block_on`), then finish its children.
pub(crate) async fn run_root(instr: Instr, env: AnyEnv, exec: Arc<dyn Executor>) -> AnyExit {
  let ctx = FiberContext::new(None, exec);
  let exit = Runloop::new(instr, env, Arc::clone(&ctx)).await;
  finish_children(&ctx).await;
  ctx.mark_done();
  exit
}

/// A handle to a running fiber. `A`/`E` need not be `Clone` or `Sync` (the slot is one-shot).
pub struct FiberHandle<A, E> {
  ctx: Arc<FiberContext>,
  cell: Cell,
  _p: PhantomData<fn() -> (A, E)>,
}

impl<A, E> std::fmt::Debug for FiberHandle<A, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("FiberHandle").field(&self.ctx.id).finish()
  }
}

/// Future of a fiber's exit.
pub struct JoinFuture<A, E> {
  cell: Cell,
  _p: PhantomData<fn() -> (A, E)>,
}

impl<A: 'static, E: 'static> Future for JoinFuture<A, E> {
  type Output = Exit<A, E>;
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    self.cell.poll_take(cx).map(unerase_exit::<A, E>)
  }
}

impl<A: Send + 'static, E: Send + 'static> FiberHandle<A, E> {
  pub(crate) fn new(ctx: Arc<FiberContext>, cell: Cell) -> Self {
    Self {
      ctx,
      cell,
      _p: PhantomData,
    }
  }

  pub fn id(&self) -> FiberId {
    self.ctx.id
  }

  pub fn is_done(&self) -> bool {
    self.ctx.is_done()
  }

  /// Await the fiber's exit from plain async code (or inside an `effect!` body).
  pub fn join_future(self) -> JoinFuture<A, E> {
    JoinFuture {
      cell: self.cell,
      _p: PhantomData,
    }
  }

  /// Wait for the fiber and re-raise its outcome. Interrupting the joiner does not interrupt the
  /// joined fiber (Effect.ts semantics).
  pub fn join<R: Send + Sync + 'static>(self) -> Effect<A, E, R> {
    let cell = self.cell;
    Effect::from_instr(Instr::Async(Box::new(move |_, _| {
      Box::pin(CellFuture(cell))
    })))
  }

  /// Request interruption without waiting.
  pub fn interrupt_now(&self) {
    self.ctx.request_interrupt();
  }

  /// Request interruption and wait for the fiber's exit (Effect.ts `Fiber.interrupt`).
  pub fn interrupt<E2: Send + 'static, R: Send + Sync + 'static>(
    self,
  ) -> Effect<Exit<A, E>, E2, R> {
    let FiberHandle { ctx, cell, .. } = self;
    crate::effect::from_async(move || async move {
      ctx.request_interrupt();
      Ok(
        JoinFuture::<A, E> {
          cell,
          _p: PhantomData,
        }
        .await,
      )
    })
  }
}

/// The erased exit of a fiber, as a future.
pub(crate) struct CellFuture(pub(crate) Cell);

impl Future for CellFuture {
  type Output = AnyExit;
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    self.0.poll_take(cx)
  }
}
