//! `FiberContext`: per-fiber state (id, interrupt flag, mask, waker, parent and children, fiber
//! refs). Filled by `k-core-runloop`, then extended by `k-fiber-tree`, `k-scope-fiberref` and
//! `k-runloop-opt`.
//!
//! `k-core-runloop` lands the minimal state the runloop needs: the id, the executor, the
//! interrupt flag, the mask counter and the stored waker. `k-fiber-tree` adds `parent: Weak`,
//! the children and the done signal (k-rebuild L2: parentage lives here, not in `FiberId`).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Waker;

use crate::next::runtime::Executor;
use crate::runtime::FiberId;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Per-fiber runtime state shared by its runloop(s), its handle and (later) its parent.
pub struct FiberContext {
  id: FiberId,
  exec: Arc<dyn Executor>,
  interrupt: AtomicBool,
  mask: AtomicU32,
  waker: Mutex<Option<Waker>>,
}

impl std::fmt::Debug for FiberContext {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FiberContext")
      .field("id", &self.id)
      .field("mask", &self.mask())
      .field("interrupt_requested", &self.is_interrupt_requested())
      .finish_non_exhaustive()
  }
}

impl FiberContext {
  /// A fresh root fiber (no parent) running on `exec`.
  pub fn new_root(exec: Arc<dyn Executor>) -> Arc<Self> {
    Arc::new(Self {
      id: FiberId::fresh(),
      exec,
      interrupt: AtomicBool::new(false),
      mask: AtomicU32::new(0),
      waker: Mutex::new(None),
    })
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
}
