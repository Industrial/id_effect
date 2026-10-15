//! `FiberContext`: per-fiber state (id, interrupt flag, mask, waker, parent and children, fiber
//! refs). Filled by `k-core-runloop`, then extended by `k-fiber-tree`, `k-scope-fiberref` and
//! `k-runloop-opt`.
//!
//! `k-core-runloop` lands the minimal state the runloop needs: the id, the executor, the
//! interrupt flag, the mask counter and the stored waker. `k-fiber-tree` adds `parent: Weak`,
//! the children and the done signal (k-rebuild L2: parentage lives here, not in `FiberId`).
//! `k-scope-fiberref` adds the fiber-ref slots (`refs`, design A §1.3): a per-fiber map from
//! [`FiberRef`](crate::next::runtime::FiberRef) id to value, copied into a child when it is
//! forked (no thread-locals).

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context, Poll, Waker};

use crate::failure::FiberId;
use crate::next::runtime::Executor;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fiber-ref value slot (an `Arc<T>` behind `dyn Any`).
pub(crate) type RefSlot = Arc<dyn Any + Send + Sync>;

/// The fiber-ref slots of one fiber: `FiberRef` id → current value.
type FiberRefs = HashMap<u64, RefSlot>;

/// Per-fiber runtime state shared by its runloop(s), its handle and (later) its parent.
pub struct FiberContext {
  id: FiberId,
  exec: Arc<dyn Executor>,
  interrupt: AtomicBool,
  mask: AtomicU32,
  waker: Mutex<Option<Waker>>,
  /// `None` for roots and daemons.
  parent: Option<Weak<FiberContext>>,
  /// Live children (each removes itself when it completes; `finish_children` drains them).
  children: Mutex<HashMap<FiberId, Arc<FiberContext>>>,
  done: AtomicBool,
  done_wakers: Mutex<Vec<Waker>>,
  /// Fiber-local values set through `FiberRef`s (an unset ref reads its initial value).
  refs: Mutex<FiberRefs>,
}

impl std::fmt::Debug for FiberContext {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FiberContext")
      .field("id", &self.id)
      .field("mask", &self.mask())
      .field("interrupt_requested", &self.is_interrupt_requested())
      .field("parent", &self.parent().map(|p| p.id()))
      .field("live_children", &self.live_children())
      .field("done", &self.is_done())
      .finish_non_exhaustive()
  }
}

impl FiberContext {
  /// A fresh root fiber (no parent) running on `exec`.
  pub fn new_root(exec: Arc<dyn Executor>) -> Arc<Self> {
    Self::with_parent(None, exec)
  }

  /// A fresh child of `parent`, on the parent's executor, starting with a copy of the parent's
  /// fiber refs. The child is not registered in `parent`'s children here: the fork path does
  /// that when it spawns the child, so a context built and never spawned does not hold up the
  /// parent's exit.
  pub fn new_child(parent: &Arc<Self>) -> Arc<Self> {
    Self::with_parent(Some(parent), Arc::clone(&parent.exec))
  }

  /// A fresh fiber with no parent that inherits `forker`'s executor and fiber refs (what
  /// `Scope::fork_in` and `fork_daemon` build: the scope, or nothing, bounds its lifetime, not
  /// the forking fiber).
  pub(crate) fn new_detached(forker: &Arc<Self>) -> Arc<Self> {
    let ctx = Self::with_parent(None, Arc::clone(&forker.exec));
    *lock(&ctx.refs) = forker.refs_snapshot();
    ctx
  }

  fn with_parent(parent: Option<&Arc<Self>>, exec: Arc<dyn Executor>) -> Arc<Self> {
    let refs = parent.map(|p| p.refs_snapshot()).unwrap_or_default();
    Arc::new(Self {
      id: FiberId::fresh(),
      exec,
      interrupt: AtomicBool::new(false),
      mask: AtomicU32::new(0),
      waker: Mutex::new(None),
      parent: parent.map(Arc::downgrade),
      children: Mutex::new(HashMap::new()),
      done: AtomicBool::new(false),
      done_wakers: Mutex::new(Vec::new()),
      refs: Mutex::new(refs),
    })
  }

  /// A copy of this fiber's ref slots. Values are shared `Arc`s and a later `set` on either
  /// side replaces the slot, so neither fiber sees the other's writes.
  fn refs_snapshot(&self) -> FiberRefs {
    lock(&self.refs).clone()
  }

  /// The slot of fiber ref `id` in this fiber, if it was set here or inherited.
  pub(crate) fn ref_get(&self, id: u64) -> Option<RefSlot> {
    lock(&self.refs).get(&id).cloned()
  }

  /// Sets fiber ref `id` in this fiber and returns the previous slot.
  pub(crate) fn ref_set(&self, id: u64, v: RefSlot) -> Option<RefSlot> {
    lock(&self.refs).insert(id, v)
  }

  /// Restores slot `id` to `prev` (`None` clears it, so the ref reads its initial value).
  pub(crate) fn ref_restore(&self, id: u64, prev: Option<RefSlot>) {
    let mut refs = lock(&self.refs);
    match prev {
      Some(v) => refs.insert(id, v),
      None => refs.remove(&id),
    };
  }

  /// This fiber's id (the one `Cause::Interrupt` carries).
  pub fn id(&self) -> FiberId {
    self.id
  }

  /// The executor this fiber runs on.
  pub fn executor(&self) -> &Arc<dyn Executor> {
    &self.exec
  }

  /// The current mask depth (`0` = interruptible).
  pub(crate) fn mask(&self) -> u32 {
    self.mask.load(Ordering::Relaxed)
  }

  pub(crate) fn set_mask(&self, m: u32) {
    self.mask.store(m, Ordering::Relaxed);
  }

  /// `true` once [`request_interrupt`](Self::request_interrupt) was called, masked or not.
  pub fn is_interrupt_requested(&self) -> bool {
    self.interrupt.load(Ordering::Acquire)
  }

  /// An interrupt was requested and the fiber is not in an uninterruptible region.
  #[inline]
  pub(crate) fn should_interrupt(&self) -> bool {
    self.is_interrupt_requested() && self.mask() == 0
  }

  /// Stores the waker of the task currently suspended on this fiber (replaced only when it
  /// would wake a different task).
  pub(crate) fn register_waker(&self, w: &Waker) {
    let mut slot = lock(&self.waker);
    match slot.as_ref() {
      Some(old) if old.will_wake(w) => {}
      _ => *slot = Some(w.clone()),
    }
  }

  /// Requests interruption and wakes the fiber if it is suspended (no polling, no spin). The
  /// runloop honours it at its next interrupt point outside a mask region.
  pub fn request_interrupt(&self) {
    self.interrupt.store(true, Ordering::Release);
    let w = lock(&self.waker).take();
    if let Some(w) = w {
      w.wake();
    }
  }

  /// The parent fiber while it is alive (`None` for roots and daemons).
  pub fn parent(&self) -> Option<Arc<FiberContext>> {
    self.parent.as_ref().and_then(Weak::upgrade)
  }

  /// How many children are registered and not yet complete.
  pub fn live_children(&self) -> usize {
    lock(&self.children).len()
  }

  /// `true` once the fiber has completed, after its own children.
  pub fn is_done(&self) -> bool {
    self.done.load(Ordering::Acquire)
  }

  pub(crate) fn add_child(&self, child: &Arc<FiberContext>) {
    lock(&self.children).insert(child.id, Arc::clone(child));
  }

  pub(crate) fn remove_child(&self, id: FiberId) {
    lock(&self.children).remove(&id);
  }

  /// Removes and returns every registered child.
  pub(crate) fn drain_children(&self) -> Vec<Arc<FiberContext>> {
    lock(&self.children).drain().map(|(_, c)| c).collect()
  }

  /// Marks the fiber complete and wakes every [`Done`] waiting on it.
  pub(crate) fn mark_done(&self) {
    self.done.store(true, Ordering::Release);
    let ws = std::mem::take(&mut *lock(&self.done_wakers));
    for w in ws {
      w.wake();
    }
  }

  fn poll_done(&self, cx: &mut Context<'_>) -> Poll<()> {
    if self.is_done() {
      return Poll::Ready(());
    }
    {
      let mut ws = lock(&self.done_wakers);
      if !ws.iter().any(|w| w.will_wake(cx.waker())) {
        ws.push(cx.waker().clone());
      }
    }
    // Re-checked after registering, so a `mark_done` in between is not missed.
    if self.is_done() {
      Poll::Ready(())
    } else {
      Poll::Pending
    }
  }
}

/// Resolves once the fiber has completed (after its own children).
#[must_use = "futures do nothing unless polled"]
#[derive(Debug)]
pub(crate) struct Done(pub(crate) Arc<FiberContext>);

impl Future for Done {
  type Output = ();
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    self.0.poll_done(cx)
  }
}
