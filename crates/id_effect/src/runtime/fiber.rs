//! Fibers: the one-shot `ExitCell`, `spawn_fiber`, `finish_children`, `FiberHandle` and
//! `JoinFuture`. Filled by `k-fiber-tree`.
//!
//! Design A §1.3 / ADR 0011 "Interruption model": every fiber is one runloop future spawned on
//! its executor. A fiber completes a one-shot [`ExitCell`] (written once by the fiber, taken once
//! by the joiner), so [`FiberHandle::join`] needs neither `Clone` nor `Sync` on `A` or `E`. When
//! a fiber's runloop ends (success, failure or interrupt) it interrupts and awaits its live
//! children ([`finish_children`]) before completing its cell. A panic that escapes the runloop
//! is caught at the fiber-poll boundary and becomes `Cause::Die`.

use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use crate::failure::Exit;
use crate::failure::FiberId;
use crate::next::capability::HNil;
use crate::next::kernel::{
  AnyEnv, AnyExit, Effect, Instr, Runloop, cause_from_panic, from_async, to_exit, unerase_exit,
};
use crate::next::runtime::{Done, Executor, FiberContext};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The one-shot completion slot: completed once by the fiber, taken once by the joiner.
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

  /// Stores the outcome and wakes the joiner. A second completion is refused (`Err(v)`).
  pub(crate) fn complete(&self, v: T) -> Result<(), T> {
    let mut s = lock(&self.state);
    match &mut *s {
      CellState::Waiting(w) => {
        let w = w.take();
        *s = CellState::Ready(v);
        drop(s);
        if let Some(w) = w {
          w.wake();
        }
        Ok(())
      }
      CellState::Ready(_) | CellState::Taken => Err(v),
    }
  }

  /// `true` once the outcome was stored (taken or not).
  pub(crate) fn is_complete(&self) -> bool {
    !matches!(*lock(&self.state), CellState::Waiting(_))
  }

  /// Takes the outcome once it is ready (registering the waker otherwise).
  ///
  /// # Panics
  ///
  /// When the outcome was already taken (a kernel bug: handles are consumed by `join`).
  pub(crate) fn poll_take(&self, cx: &mut Context<'_>) -> Poll<T> {
    let mut s = lock(&self.state);
    match std::mem::replace(&mut *s, CellState::Taken) {
      CellState::Ready(v) => Poll::Ready(v),
      CellState::Waiting(old) => {
        let w = match old {
          Some(w) if w.will_wake(cx.waker()) => w,
          _ => cx.waker().clone(),
        };
        *s = CellState::Waiting(Some(w));
        Poll::Pending
      }
      CellState::Taken => panic!("next: fiber exit taken twice (kernel bug)"),
    }
  }
}

/// A fiber's erased exit slot.
pub(crate) type FiberCell = Arc<ExitCell<AnyExit>>;

/// Polls `F`, turning a panic that escapes it into `Err(payload)` (the fiber-poll boundary).
struct CatchPanic<F>(F);

impl<F: Future + Unpin> Future for CatchPanic<F> {
  type Output = Result<F::Output, Box<dyn Any + Send>>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let inner = &mut self.0;
    match catch_unwind(AssertUnwindSafe(|| Pin::new(inner).poll(cx))) {
      Ok(Poll::Pending) => Poll::Pending,
      Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
      Err(payload) => Poll::Ready(Err(payload)),
    }
  }
}

/// Interrupts every live child of `ctx` and awaits each one's completion (structured
/// concurrency). Loops until no child is left, so children registered meanwhile are included.
pub(crate) async fn finish_children(ctx: &Arc<FiberContext>) {
  loop {
    let kids = ctx.drain_children();
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

/// Spawns `instr` as the fiber `ctx` on `ctx`'s executor and returns its exit slot.
///
/// When `ctx` has a parent (see [`FiberContext::new_child`]) the fiber is registered in the
/// parent's children first, so the parent interrupts and awaits it on exit; a root context
/// ([`FiberContext::new_root`]) makes a daemon. The task runs the runloop, then
/// [`finish_children`], then leaves its parent, marks `ctx` done and completes the cell.
pub(crate) fn spawn_fiber(ctx: &Arc<FiberContext>, instr: Instr, env: AnyEnv) -> FiberCell {
  if let Some(p) = ctx.parent() {
    p.add_child(ctx);
  }
  let cell: FiberCell = Arc::new(ExitCell::new());
  let task = {
    let ctx = Arc::clone(ctx);
    let cell = Arc::clone(&cell);
    async move {
      let exit = match CatchPanic(Runloop::new(instr, env, Arc::clone(&ctx))).await {
        Ok(exit) => exit,
        Err(payload) => Err(cause_from_panic(payload)),
      };
      finish_children(&ctx).await;
      if let Some(p) = ctx.parent() {
        p.remove_child(ctx.id());
      }
      // Done before the cell completes, so a joiner that resumes always sees `is_done()`.
      ctx.mark_done();
      // The cell is fresh and only this task completes it.
      let _ = cell.complete(exit);
    }
  };
  ctx.executor().spawn(Box::pin(task));
  cell
}

impl<A: Send + 'static, E: Send + 'static> Effect<A, E, HNil> {
  /// Drives this effect as the root fiber `fiber` on the current task, then interrupts and
  /// awaits `fiber`'s children and marks it done (the async run edge's building block).
  ///
  /// Unlike [`run_on`](Effect::run_on), the returned future does not complete before every
  /// child forked under `fiber` has completed. A panic that escapes the runloop becomes
  /// `Cause::Die`.
  pub fn run_fiber(self, fiber: Arc<FiberContext>) -> impl Future<Output = Exit<A, E>> + Send {
    let run = self.run_on(Arc::clone(&fiber));
    async move {
      let exit = match CatchPanic(run).await {
        Ok(exit) => exit,
        Err(payload) => Exit::Failure(cause_from_panic(payload)),
      };
      finish_children(&fiber).await;
      fiber.mark_done();
      exit
    }
  }

  /// Spawns this effect as a new root fiber (no parent) on `exec` and returns its handle.
  pub fn fork_root(self, exec: Arc<dyn Executor>) -> FiberHandle<A, E> {
    let ctx = FiberContext::new_root(exec);
    let cell = spawn_fiber(&ctx, self.into_instr(), Arc::new(HNil) as AnyEnv);
    FiberHandle::new(ctx, cell)
  }
}

/// A handle to a running fiber. `A` and `E` need not be `Clone` or `Sync`: the exit slot is
/// one-shot, and [`join`](Self::join) consumes the handle.
///
/// Dropping a handle does not interrupt the fiber; a child is still interrupted when its
/// parent exits.
pub struct FiberHandle<A, E> {
  ctx: Arc<FiberContext>,
  cell: FiberCell,
  _p: PhantomData<fn() -> (A, E)>,
}

impl<A, E> std::fmt::Debug for FiberHandle<A, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("FiberHandle").field(&self.ctx.id()).finish()
  }
}

impl<A: Send + 'static, E: Send + 'static> FiberHandle<A, E> {
  pub(crate) fn new(ctx: Arc<FiberContext>, cell: FiberCell) -> Self {
    Self {
      ctx,
      cell,
      _p: PhantomData,
    }
  }

  /// The fiber's id (the one its `Cause::Interrupt` carries).
  pub fn id(&self) -> FiberId {
    self.ctx.id()
  }

  /// The fiber's context (parent, children, interrupt state).
  pub fn fiber(&self) -> &Arc<FiberContext> {
    &self.ctx
  }

  /// `true` once the fiber's outcome is available to [`join`](Self::join).
  pub fn is_done(&self) -> bool {
    self.cell.is_complete()
  }

  /// Awaits the fiber's exit from plain async code.
  pub fn join_future(self) -> JoinFuture<A, E> {
    JoinFuture {
      cell: self.cell,
      _p: PhantomData,
    }
  }

  /// Waits for the fiber and re-raises its outcome (an interrupted fiber re-raises
  /// `Cause::Interrupt` with its own id). Interrupting the joiner does not interrupt the joined
  /// fiber (Effect.ts semantics).
  pub fn join<R: Send + Sync + 'static>(self) -> Effect<A, E, R> {
    Effect::from_instr(Instr::Future(Box::pin(CellFuture(self.cell))))
  }

  /// Requests interruption without waiting.
  pub fn interrupt_now(&self) {
    self.ctx.request_interrupt();
  }

  /// Requests interruption and waits for the fiber's exit (Effect.ts `Fiber.interrupt`). The
  /// fiber's finalizers and children have completed when this succeeds.
  pub fn interrupt<E2: Send + 'static, R: Send + Sync + 'static>(
    self,
  ) -> Effect<Exit<A, E>, E2, R> {
    let FiberHandle { ctx, cell, .. } = self;
    from_async(move || async move {
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

/// The future of a fiber's exit ([`FiberHandle::join_future`]).
#[must_use = "futures do nothing unless polled"]
pub struct JoinFuture<A, E> {
  cell: FiberCell,
  _p: PhantomData<fn() -> (A, E)>,
}

impl<A, E> std::fmt::Debug for JoinFuture<A, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("JoinFuture")
  }
}

impl<A: 'static, E: 'static> Future for JoinFuture<A, E> {
  type Output = Exit<A, E>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    self
      .cell
      .poll_take(cx)
      .map(|exit| to_exit(unerase_exit::<A, E>(exit)))
  }
}

/// The erased exit of a fiber, as a future (what `join` suspends on).
pub(crate) struct CellFuture(pub(crate) FiberCell);

impl Future for CellFuture {
  type Output = AnyExit;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    self.0.poll_take(cx)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::task::Wake;

  use super::*;

  struct CountWake(AtomicUsize);

  impl Wake for CountWake {
    fn wake(self: Arc<Self>) {
      self.0.fetch_add(1, Ordering::SeqCst);
    }
  }

  #[test]
  fn exit_cell_completes_once_and_is_taken_once() {
    let counter = Arc::new(CountWake(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&counter));
    let mut cx = Context::from_waker(&waker);
    let cell = ExitCell::<u8>::new();
    assert!(cell.poll_take(&mut cx).is_pending());
    assert!(!cell.is_complete());
    assert_eq!(cell.complete(1), Ok(()));
    assert_eq!(counter.0.load(Ordering::SeqCst), 1, "joiner woken once");
    assert_eq!(cell.complete(2), Err(2), "second completion refused");
    assert_eq!(cell.poll_take(&mut cx), Poll::Ready(1));
    assert!(cell.is_complete());
    let again = catch_unwind(AssertUnwindSafe(|| cell.poll_take(&mut cx)));
    assert!(again.is_err(), "second take is a kernel bug");
  }
}
