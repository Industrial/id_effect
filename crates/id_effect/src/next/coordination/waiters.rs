//! Crate-internal building blocks shared by the coordination primitives: a poison-tolerant
//! `lock`, a FIFO [`WaitList`] of suspended waiters with hand-off grants, and small effect
//! helpers (`fail_cause`, `with_fiber_id`, `from_cause_future`).
//!
//! # The wait list
//!
//! A suspended operation (acquire, take, offer, wait) pushes an entry (`id`, payload, [`Waker`])
//! and returns `Pending`. The side that makes progress possible pops the front entry, stores a
//! **grant** for its id (the permit, the item, an accept/reject decision) and wakes it, all under
//! the primitive's one mutex. The woken future takes its grant on its next poll.
//!
//! Interruption drops the suspended future. Its `Drop` removes its entry, or, when a grant was
//! already stored but not yet observed, hands the granted value back to the primitive (so an
//! interrupted waiter never leaks a permit or an item).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Waker;

use crate::failure::Cause;
use crate::failure::FiberId;
use crate::next::capability::HNil;
use crate::next::kernel::{AnyEnv, Effect, Instr, erase_exit};

/// Locks `m`, recovering the guard from a poisoned mutex (a panicking user closure in an update
/// must not wedge the primitive for every other fiber).
pub(crate) fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Wakes every waker in `ws` (call after the primitive's lock is released).
pub(crate) fn wake_all(ws: Vec<Waker>) {
  for w in ws {
    w.wake();
  }
}

/// An effect that fails with the full `cause` (defects and interrupts included).
pub(crate) fn fail_cause<A, E, R>(cause: Cause<E>) -> Effect<A, E, R> {
  Effect::pure(Err(cause))
}

/// Builds an effect from the id of the fiber running it.
pub(crate) fn with_fiber_id<A, E, R>(
  f: impl FnOnce(FiberId) -> Effect<A, E, R> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    f(fiber.id()).into_instr()
  })))
}

/// Runs an effect that needs no capabilities under any environment `R`.
pub(crate) fn hnil_in<A, E, R>(eff: Effect<A, E, HNil>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::Provide(
    Box::new(eff.into_instr()),
    Box::new(|_| Arc::new(HNil) as AnyEnv),
  ))
}

/// An async step whose future yields a full `Result<A, Cause<E>>` (so a stored cause is re-raised
/// as is, not wrapped in `Cause::Fail`). `make` runs on first poll.
pub(crate) fn from_cause_future<A, E, R, F, Fut>(make: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnOnce() -> Fut + Send + 'static,
  Fut: Future<Output = Result<A, Cause<E>>> + Send + 'static,
{
  Effect::from_instr(Instr::Future(Box::pin(
    async move { erase_exit(make().await) },
  )))
}

/// A queued waiter.
struct Entry<P> {
  id: u64,
  payload: P,
  waker: Waker,
}

/// A FIFO list of suspended waiters carrying a payload `P` (a permit count, an item to offer, …),
/// plus the grants `T` handed to waiters that were popped but have not observed them yet.
pub(crate) struct WaitList<P, T> {
  next: u64,
  queue: VecDeque<Entry<P>>,
  granted: HashMap<u64, T>,
}

impl<P, T> Default for WaitList<P, T> {
  fn default() -> Self {
    Self {
      next: 0,
      queue: VecDeque::new(),
      granted: HashMap::new(),
    }
  }
}

impl<P, T> WaitList<P, T> {
  /// `true` when no waiter is queued (grants not yet observed are not counted).
  pub(crate) fn is_empty(&self) -> bool {
    self.queue.is_empty()
  }

  /// Number of queued waiters.
  pub(crate) fn len(&self) -> usize {
    self.queue.len()
  }

  /// Queues a waiter at the back and returns its id.
  pub(crate) fn push(&mut self, payload: P, waker: &Waker) -> u64 {
    let id = self.next;
    self.next += 1;
    self.queue.push_back(Entry {
      id,
      payload,
      waker: waker.clone(),
    });
    id
  }

  /// The payload of the front waiter.
  pub(crate) fn front(&self) -> Option<&P> {
    self.queue.front().map(|e| &e.payload)
  }

  /// Pops the front waiter (`id`, payload, waker).
  pub(crate) fn pop_front(&mut self) -> Option<(u64, P, Waker)> {
    self.queue.pop_front().map(|e| (e.id, e.payload, e.waker))
  }

  /// Stores `v` for the (already popped) waiter `id`.
  pub(crate) fn grant(&mut self, id: u64, v: T) {
    self.granted.insert(id, v);
  }

  /// Pops the front waiter, grants it `v` and returns its waker.
  pub(crate) fn grant_front(&mut self, v: T) -> Result<(P, Waker), T> {
    match self.pop_front() {
      Some((id, p, w)) => {
        self.grant(id, v);
        Ok((p, w))
      }
      None => Err(v),
    }
  }

  /// Takes the grant stored for `id`, if any.
  pub(crate) fn take_grant(&mut self, id: u64) -> Option<T> {
    self.granted.remove(&id)
  }

  /// Removes the queued waiter `id`, returning its payload (`None` when it is not queued).
  pub(crate) fn remove(&mut self, id: u64) -> Option<P> {
    let pos = self.queue.iter().position(|e| e.id == id)?;
    self.queue.remove(pos).map(|e| e.payload)
  }

  /// Refreshes the waker of the queued waiter `id`; `false` when it is no longer queued.
  pub(crate) fn refresh(&mut self, id: u64, waker: &Waker) -> bool {
    match self.queue.iter_mut().find(|e| e.id == id) {
      Some(e) => {
        if !e.waker.will_wake(waker) {
          e.waker = waker.clone();
        }
        true
      }
      None => false,
    }
  }

  /// Pops every queued waiter, in order.
  pub(crate) fn drain(&mut self) -> Vec<(u64, P, Waker)> {
    self
      .queue
      .drain(..)
      .map(|e| (e.id, e.payload, e.waker))
      .collect()
  }
}

impl<P> WaitList<P, ()> {
  /// Grants every queued waiter `()` and returns their wakers (a broadcast wake-up).
  pub(crate) fn release_all(&mut self) -> Vec<Waker> {
    let all = self.drain();
    all
      .into_iter()
      .map(|(id, _, w)| {
        self.grant(id, ());
        w
      })
      .collect()
  }
}

/// A one-shot broadcast signal (`await_shutdown`): waiters suspend until it is fired once.
#[derive(Default)]
pub(crate) struct Signal {
  state: Mutex<SignalState>,
}

#[derive(Default)]
struct SignalState {
  fired: bool,
  waiters: WaitList<(), ()>,
}

impl Signal {
  /// Fires the signal and wakes every waiter. Idempotent.
  pub(crate) fn fire(&self) {
    let ws = {
      let mut st = lock(&self.state);
      st.fired = true;
      st.waiters.release_all()
    };
    wake_all(ws);
  }

  /// A future that completes once the signal has fired.
  pub(crate) fn wait(self: &Arc<Self>) -> SignalWait {
    SignalWait {
      signal: Arc::clone(self),
      id: None,
    }
  }
}

/// The future of [`Signal::wait`].
pub(crate) struct SignalWait {
  signal: Arc<Signal>,
  id: Option<u64>,
}

impl Future for SignalWait {
  type Output = ();

  fn poll(
    mut self: std::pin::Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<()> {
    let this = &mut *self;
    let mut st = lock(&this.signal.state);
    if let Some(id) = this.id {
      if st.waiters.take_grant(id).is_some() {
        this.id = None;
        return std::task::Poll::Ready(());
      }
      st.waiters.refresh(id, cx.waker());
      return std::task::Poll::Pending;
    }
    if st.fired {
      return std::task::Poll::Ready(());
    }
    this.id = Some(st.waiters.push((), cx.waker()));
    std::task::Poll::Pending
  }
}

impl Drop for SignalWait {
  fn drop(&mut self) {
    if let Some(id) = self.id.take() {
      let mut st = lock(&self.signal.state);
      if st.waiters.take_grant(id).is_none() {
        st.waiters.remove(id);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::task::{Wake, Waker};

  use super::*;

  struct Count(AtomicUsize);

  impl Wake for Count {
    fn wake(self: Arc<Self>) {
      self.0.fetch_add(1, Ordering::SeqCst);
    }
  }

  #[test]
  fn wait_list_is_fifo_and_grants_by_id() {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    let w = &Waker::from(Arc::clone(&count));
    let mut l: WaitList<u8, &str> = WaitList::default();
    let a = l.push(1, w);
    let b = l.push(2, w);
    assert_eq!(l.len(), 2);
    assert_eq!(l.front(), Some(&1));
    let Ok((payload, waker)) = l.grant_front("x") else {
      panic!("a waiter is queued")
    };
    assert_eq!(payload, 1);
    wake_all(vec![waker]);
    assert_eq!(count.0.load(Ordering::SeqCst), 1);
    assert_eq!(l.take_grant(a), Some("x"));
    assert_eq!(l.take_grant(a), None);
    assert!(l.refresh(b, w));
    assert_eq!(l.remove(b), Some(2));
    assert!(!l.refresh(b, w));
    assert!(l.is_empty());
    assert_eq!(l.grant_front("y").err(), Some("y"));
  }
}
