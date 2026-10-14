//! Fibers (design §1.3/§1.4): `FiberState` (parent/child tree, interrupt flag, mask, waker),
//! the one-shot `ExitCell` (R1: no `watch`), `FiberHandle`, fork/join/interrupt, masks and
//! finalizers.

use std::cell::RefCell;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::cause::{Cause, Defect, Exit, Never};
use crate::executor::Executor;
use crate::kernel::{Effect, box_future};

/// A fiber's identity (kept from 0.6 `concurrency/fiber_id.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FiberId(pub u64);

static NEXT_FIBER: AtomicU64 = AtomicU64::new(1);

struct Children {
  list: Vec<Arc<FiberState>>,
  prune_at: usize,
}

pub(crate) struct FiberState {
  pub(crate) id: FiberId,
  pub(crate) exec: Arc<dyn Executor>,
  interrupted: AtomicBool,
  mask: AtomicU32,
  waker: Mutex<Option<Waker>>,
  children: Mutex<Children>,
  done: AtomicBool,
  done_waiters: Mutex<Vec<Waker>>,
}

impl FiberState {
  fn new(exec: Arc<dyn Executor>) -> Arc<Self> {
    Arc::new(Self {
      id: FiberId(NEXT_FIBER.fetch_add(1, Ordering::Relaxed)),
      exec,
      interrupted: AtomicBool::new(false),
      mask: AtomicU32::new(0),
      waker: Mutex::new(None),
      children: Mutex::new(Children {
        list: Vec::new(),
        prune_at: 16,
      }),
      done: AtomicBool::new(false),
      done_waiters: Mutex::new(Vec::new()),
    })
  }

  fn should_interrupt(&self) -> bool {
    self.interrupted.load(Ordering::Acquire) && self.mask.load(Ordering::Acquire) == 0
  }

  pub(crate) fn request_interrupt(&self) {
    self.interrupted.store(true, Ordering::Release);
    let w = self.waker.lock().expect("fiber waker lock").take();
    if let Some(w) = w {
      w.wake();
    }
  }

  pub(crate) fn is_done(&self) -> bool {
    self.done.load(Ordering::Acquire)
  }

  fn add_child(&self, child: Arc<FiberState>) {
    let mut c = self.children.lock().expect("fiber children lock");
    if c.list.len() >= c.prune_at {
      c.list.retain(|k| !k.is_done());
      c.prune_at = (c.list.len() * 2).max(16);
    }
    c.list.push(child);
  }

  fn take_children(&self) -> Vec<Arc<FiberState>> {
    std::mem::take(&mut self.children.lock().expect("fiber children lock").list)
  }

  fn finish(&self) {
    self.done.store(true, Ordering::Release);
    let waiters = std::mem::take(&mut *self.done_waiters.lock().expect("fiber done lock"));
    for w in waiters {
      w.wake();
    }
  }

  fn store_waker(&self, cx: &Context<'_>) {
    let mut slot = self.waker.lock().expect("fiber waker lock");
    if !slot.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
      *slot = Some(cx.waker().clone());
    }
  }
}

thread_local! {
  static CURRENT: RefCell<Option<Arc<FiberState>>> = const { RefCell::new(None) };
}

/// The fiber being polled on this thread, if any.
pub(crate) fn current() -> Option<Arc<FiberState>> {
  CURRENT.with(|c| c.borrow().clone())
}

/// `true` when called from inside a fiber poll.
pub fn in_fiber() -> bool {
  CURRENT.with(|c| c.borrow().is_some())
}

/// `Some(id)` if the current fiber has an unmasked interrupt request (an interrupt point).
#[inline]
pub(crate) fn interrupt_requested() -> Option<FiberId> {
  CURRENT.with(|c| {
    c.borrow()
      .as_ref()
      .and_then(|s| s.should_interrupt().then_some(s.id))
  })
}

/// Poll `inner` with `state` installed as the current fiber; catch panics as defects.
struct WithFiber<F> {
  state: Arc<FiberState>,
  inner: F,
}

impl<F: Future + Unpin> Future for WithFiber<F> {
  type Output = Result<F::Output, Defect>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let this = &mut *self;
    this.state.store_waker(cx);
    let prev = CURRENT.with(|c| c.replace(Some(Arc::clone(&this.state))));
    let out = catch_unwind(AssertUnwindSafe(|| Pin::new(&mut this.inner).poll(cx)));
    CURRENT.with(|c| *c.borrow_mut() = prev);
    match out {
      Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
      Ok(Poll::Pending) => Poll::Pending,
      Err(payload) => Poll::Ready(Err(Defect::from_panic(payload))),
    }
  }
}

/// Wait until a fiber has finished (not an interrupt point by itself).
pub(crate) struct WaitDone(pub(crate) Arc<FiberState>);

impl Future for WaitDone {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    if self.0.is_done() {
      return Poll::Ready(());
    }
    self
      .0
      .done_waiters
      .lock()
      .expect("fiber done lock")
      .push(cx.waker().clone());
    if self.0.is_done() {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  }
}

/// Wait until the first of two fibers finishes; `0` or `1`.
pub(crate) struct WaitFirst(pub(crate) Arc<FiberState>, pub(crate) Arc<FiberState>);

impl Future for WaitFirst {
  type Output = usize;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
    let check = |this: &Self| {
      if this.0.is_done() {
        Some(0)
      } else if this.1.is_done() {
        Some(1)
      } else {
        None
      }
    };
    if let Some(i) = check(&self) {
      return Poll::Ready(i);
    }
    for s in [&self.0, &self.1] {
      s.done_waiters
        .lock()
        .expect("fiber done lock")
        .push(cx.waker().clone());
    }
    check(&self).map_or(Poll::Pending, Poll::Ready)
  }
}

/// `Err(id)` instead of waiting further once the current fiber has an unmasked interrupt.
pub(crate) struct Interruptibly<F>(pub(crate) F);

impl<F: Future + Unpin> Future for Interruptibly<F> {
  type Output = Result<F::Output, FiberId>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    match Pin::new(&mut self.0).poll(cx) {
      Poll::Ready(v) => Poll::Ready(Ok(v)),
      Poll::Pending => interrupt_requested().map_or(Poll::Pending, |id| Poll::Ready(Err(id))),
    }
  }
}

/// The one-shot exit slot (R1). `join` takes the value, so `A`/`E` need not be `Clone`.
pub(crate) struct ExitCell<A, E>(Mutex<Option<Exit<A, E>>>);

impl<A, E> ExitCell<A, E> {
  fn put(&self, exit: Exit<A, E>) {
    *self.0.lock().expect("exit cell lock") = Some(exit);
  }

  pub(crate) fn take(&self) -> Exit<A, E> {
    self
      .0
      .lock()
      .expect("exit cell lock")
      .take()
      .unwrap_or_else(|| {
        Err(Cause::Die(Defect::new(
          "kernel_b: fiber exit already taken",
        )))
      })
  }
}

/// A handle to a running fiber.
pub struct FiberHandle<A, E> {
  pub(crate) state: Arc<FiberState>,
  pub(crate) cell: Arc<ExitCell<A, E>>,
}

impl<A, E> Clone for FiberHandle<A, E> {
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
      cell: Arc::clone(&self.cell),
    }
  }
}

impl<A: Send + 'static, E: Send + 'static> FiberHandle<A, E> {
  pub fn id(&self) -> FiberId {
    self.state.id
  }

  pub fn is_done(&self) -> bool {
    self.state.is_done()
  }

  /// Await the fiber and take its result (an interrupt point for the joiner).
  pub fn join<R: Send + 'static>(&self) -> Effect<A, E, R> {
    let h = self.clone();
    Effect::from_fn_async(move |_r| {
      box_future(async move {
        WaitDone(Arc::clone(&h.state)).await;
        h.cell.take()
      })
    })
  }

  /// Request interruption and **await** the fiber's exit (Effect.ts semantics).
  pub fn interrupt<R: Send + 'static>(&self) -> Effect<Exit<A, E>, Never, R> {
    let h = self.clone();
    Effect::structured(move |_r| {
      box_future(async move {
        h.state.request_interrupt();
        WaitDone(Arc::clone(&h.state)).await;
        Ok(h.cell.take())
      })
    })
  }

  /// Request interruption without waiting.
  pub fn interrupt_fork(&self) {
    self.state.request_interrupt();
  }

  /// The fiber's exit as a plain future (edge use: `Runtime`, benches).
  pub async fn exit(self) -> Exit<A, E> {
    WaitDone(Arc::clone(&self.state)).await;
    self.cell.take()
  }
}

/// Spawn `eff` with `env` as a new fiber on `exec`. Non-daemon fibers are registered as children
/// of `parent`, which interrupts and awaits them when it exits (structured concurrency).
pub(crate) fn spawn_fiber<A, E, R>(
  exec: Arc<dyn Executor>,
  parent: Option<&Arc<FiberState>>,
  eff: Effect<A, E, R>,
  env: R,
) -> FiberHandle<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
{
  let state = FiberState::new(Arc::clone(&exec));
  if let Some(p) = parent {
    p.add_child(Arc::clone(&state));
  }
  let cell = Arc::new(ExitCell(Mutex::new(None)));
  let fut = fiber_main(Arc::clone(&state), Arc::clone(&cell), eff, env);
  exec.spawn(Box::pin(fut));
  FiberHandle { state, cell }
}

async fn fiber_main<A, E, R>(
  state: Arc<FiberState>,
  cell: Arc<ExitCell<A, E>>,
  eff: Effect<A, E, R>,
  mut env: R,
) where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
{
  let exit = if state.should_interrupt() {
    Err(Cause::Interrupt(state.id))
  } else {
    let run = WithFiber {
      state: Arc::clone(&state),
      inner: eff.run_in(&mut env),
    };
    match run.await {
      Ok(exit) => exit,
      Err(defect) => Err(Cause::Die(defect)),
    }
  };
  drop(env);
  // Structured concurrency: interrupt and await every live child before completing.
  loop {
    let kids = state.take_children();
    if kids.is_empty() {
      break;
    }
    for kid in kids {
      if !kid.is_done() {
        kid.request_interrupt();
        WaitDone(kid).await;
      }
    }
  }
  cell.put(exit);
  state.finish();
}

/// Spawn a root fiber (no parent) on `exec`.
pub(crate) fn spawn_root<A, E, R>(
  exec: Arc<dyn Executor>,
  eff: Effect<A, E, R>,
  env: R,
) -> FiberHandle<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
{
  spawn_fiber(exec, None, eff, env)
}

/// Increments the current fiber's uninterruptible depth until dropped.
pub(crate) struct MaskGuard(Option<Arc<FiberState>>);

impl MaskGuard {
  pub(crate) fn enter() -> Self {
    let cur = current();
    if let Some(s) = &cur {
      s.mask.fetch_add(1, Ordering::AcqRel);
    }
    Self(cur)
  }
}

impl Drop for MaskGuard {
  fn drop(&mut self) {
    if let Some(s) = &self.0 {
      s.mask.fetch_sub(1, Ordering::AcqRel);
    }
  }
}

fn no_fiber<T>(what: &str) -> Result<T, Cause<Never>> {
  Err(Cause::Die(Defect::new(format!(
    "kernel_b: {what} outside a fiber (run the effect with block_on/Runtime)"
  ))))
}

impl<A: Send + 'static, E: Send + 'static, R: Clone + Send + 'static> Effect<A, E, R> {
  /// Fork as a child fiber of the current fiber, with a clone of `R`.
  pub fn fork(self) -> Effect<FiberHandle<A, E>, Never, R> {
    Effect::sync(move |r: &mut R| match current() {
      Some(parent) => Ok(spawn_fiber(
        Arc::clone(&parent.exec),
        Some(&parent),
        self,
        r.clone(),
      )),
      None => no_fiber("fork"),
    })
  }

  /// Fork attached to the root scope: not interrupted when the forking fiber exits.
  pub fn fork_daemon(self) -> Effect<FiberHandle<A, E>, Never, R> {
    Effect::sync(move |r: &mut R| match current() {
      Some(parent) => Ok(spawn_fiber(Arc::clone(&parent.exec), None, self, r.clone())),
      None => no_fiber("fork_daemon"),
    })
  }
}

impl<A: Send + 'static, E: Send + 'static, R: Send + 'static> Effect<A, E, R> {
  /// Run `self` with interrupts masked (§1.4).
  pub fn uninterruptible(self) -> Self {
    Effect::structured(move |r| {
      box_future(async move {
        let _mask = MaskGuard::enter();
        self.run_in(r).await
      })
    })
  }

  /// Run `finalizer` (masked) after `self`, whatever its exit.
  pub fn ensuring(self, finalizer: Effect<(), Never, R>) -> Self {
    Effect::structured(move |r| {
      box_future(async move {
        let out = self.run_in(&mut *r).await;
        let _mask = MaskGuard::enter();
        let _ = finalizer.run_in(r).await;
        out
      })
    })
  }
}

/// `acquire` (masked), then `use_` on the resource, then `release` (masked) whatever the exit.
pub fn acquire_use_release<T, A, E, R, U, Rel>(
  acquire: Effect<T, E, R>,
  use_: U,
  release: Rel,
) -> Effect<A, E, R>
where
  T: Clone + Send + 'static,
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
  U: FnOnce(T) -> Effect<A, E, R> + Send + 'static,
  Rel: FnOnce(T, &Exit<A, E>) -> Effect<(), Never, R> + Send + 'static,
{
  Effect::structured(move |r| {
    box_future(async move {
      let resource = {
        let _mask = MaskGuard::enter();
        acquire.run_in(&mut *r).await?
      };
      let out = use_(resource.clone()).run_in(&mut *r).await;
      let _mask = MaskGuard::enter();
      let _ = release(resource, &out).run_in(r).await;
      out
    })
  })
}

/// Sleep on the current fiber's executor timer (an interrupt point).
pub fn sleep<E: Send + 'static, R: Send + 'static>(d: std::time::Duration) -> Effect<(), E, R> {
  Effect::from_fn_async(move |_r| {
    box_future(async move {
      let timer = match current() {
        Some(s) => s.exec.sleep(d),
        None => crate::executor::timer_sleep(d),
      };
      timer.await;
      Ok(())
    })
  })
}
