//! [`TimerRef`] — cancellable delayed messages (BEAM's `erlang:send_after/3`).
//!
//! A timer is one task on the node's executor: it sleeps on the executor clock (virtual time on
//! a [`TestExecutor`](id_effect::TestExecutor)), then delivers its message unless it was
//! cancelled. Cancelling and firing race on one atomic state, so exactly one of them wins:
//! [`TimerRef::cancel`] returning `true` means the message is never delivered. A cancelled
//! timer's task ends at once (its sleep is dropped), not at the deadline.
//!
//! Every timer belongs to the process it delivers to and dies with it: finalization cancels the
//! timers the process still owns. A timer delivering a user message counts against the
//! mailbox bound like any send (M10): a full mailbox drops it.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::node::ProcessNode;
use crate::pid::Pid;

const PENDING: u8 = 0;
const CANCELLED: u8 = 1;
const FIRED: u8 = 2;

static NEXT_TIMER: AtomicU64 = AtomicU64::new(1);

struct Shared {
  state: AtomicU8,
  /// The timer task, woken by a cancel so it ends before its deadline.
  waker: Mutex<Option<Waker>>,
}

/// Handle to a pending timer from [`ProcessCtx::send_after`](crate::ProcessCtx::send_after) or
/// [`Addr::send_after`](crate::Addr::send_after). Cloning yields another handle to the same timer.
#[derive(Clone)]
pub struct TimerRef {
  id: u64,
  shared: Arc<Shared>,
}

impl fmt::Debug for TimerRef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TimerRef")
      .field("id", &self.id)
      .field("pending", &self.is_pending())
      .finish()
  }
}

impl PartialEq for TimerRef {
  fn eq(&self, other: &Self) -> bool {
    self.id == other.id
  }
}

impl Eq for TimerRef {}

impl TimerRef {
  fn with_state(state: u8) -> Self {
    Self {
      id: NEXT_TIMER.fetch_add(1, Ordering::Relaxed),
      shared: Arc::new(Shared {
        state: AtomicU8::new(state),
        waker: Mutex::new(None),
      }),
    }
  }

  /// A timer that never delivers (its target was not alive, or not of the named behavior).
  pub(crate) fn inert() -> Self {
    Self::with_state(CANCELLED)
  }

  /// Cancel the timer. `true`: it was pending and its message will never be delivered. `false`:
  /// it already fired (the message was delivered, or dropped by a full or closed mailbox) or
  /// was cancelled before, by this call's peers or by its process's exit.
  pub fn cancel(&self) -> bool {
    let won = self
      .shared
      .state
      .compare_exchange(PENDING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
      .is_ok();
    if won
      && let Some(w) = self
        .shared
        .waker
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
      w.wake();
    }
    won
  }

  /// Whether the timer has neither fired nor been cancelled.
  pub fn is_pending(&self) -> bool {
    self.shared.state.load(Ordering::Acquire) == PENDING
  }

  /// Claim the firing; `false` when a cancel won.
  fn claim_fire(&self) -> bool {
    self
      .shared
      .state
      .compare_exchange(PENDING, FIRED, Ordering::AcqRel, Ordering::Acquire)
      .is_ok()
  }
}

/// The timers a process owns; `None` once it finalized (a late timer is cancelled at once).
pub(crate) struct Timers {
  live: Mutex<Option<HashMap<u64, TimerRef>>>,
}

impl Timers {
  pub(crate) fn new() -> Self {
    Self {
      live: Mutex::new(Some(HashMap::new())),
    }
  }

  fn add(&self, timer: &TimerRef) -> bool {
    match &mut *self.live.lock().unwrap_or_else(PoisonError::into_inner) {
      Some(map) => {
        map.insert(timer.id, timer.clone());
        true
      }
      None => false,
    }
  }

  fn remove(&self, id: u64) {
    if let Some(map) = &mut *self.live.lock().unwrap_or_else(PoisonError::into_inner) {
      map.remove(&id);
    }
  }

  /// Cancel every owned timer and refuse new ones (process finalization).
  pub(crate) fn cancel_all(&self) {
    let owned = self
      .live
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take();
    for timer in owned.into_iter().flat_map(HashMap::into_values) {
      timer.cancel();
    }
  }
}

/// Start a timer owned by `owner` that runs `deliver` after `after` on `node`'s executor clock.
/// An `owner` that is not alive yields an inert timer.
pub(crate) fn start(
  node: &ProcessNode,
  owner: &Pid,
  after: Duration,
  deliver: Box<dyn FnOnce() + Send>,
) -> TimerRef {
  let Some(cell) = node.table().lookup(owner) else {
    return TimerRef::inert();
  };
  let timer = TimerRef::with_state(PENDING);
  if !cell.timers().add(&timer) {
    timer.cancel();
    return timer;
  }
  let executor = node.runtime().executor();
  let task = TimerTask {
    sleep: executor.sleep(after),
    timer: timer.clone(),
    deliver: Some(deliver),
    node: node.clone(),
    owner: owner.clone(),
  };
  executor.spawn(Box::pin(task));
  timer
}

struct TimerTask {
  sleep: Pin<Box<dyn Future<Output = ()> + Send>>,
  timer: TimerRef,
  deliver: Option<Box<dyn FnOnce() + Send>>,
  node: ProcessNode,
  owner: Pid,
}

impl TimerTask {
  fn forget(&self) {
    if let Some(cell) = self.node.table().lookup(&self.owner) {
      cell.timers().remove(self.timer.id);
    }
  }
}

impl Future for TimerTask {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    let this = self.get_mut();
    *this
      .timer
      .shared
      .waker
      .lock()
      .unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
    if this.timer.shared.state.load(Ordering::Acquire) != PENDING {
      this.forget();
      return Poll::Ready(());
    }
    if this.sleep.as_mut().poll(cx).is_pending() {
      return Poll::Pending;
    }
    if this.timer.claim_fire()
      && let Some(deliver) = this.deliver.take()
    {
      deliver();
    }
    this.forget();
    Poll::Ready(())
  }
}
