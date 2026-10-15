//! Internal waiting primitives for `Pool` and `Cache`: a FIFO permit [`Gate`] and a one-shot
//! broadcast [`Slot`], plus small kernel helpers (the executor clock, self-interrupt, running an
//! `HNil` effect under any env).
//!
//! Both primitives are cancel-safe. A waiting future that is dropped (its fiber was interrupted)
//! deregisters itself, and a [`Permit`] is an RAII guard: an interrupt that drops it at any
//! instruction boundary gives the capacity back.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

use crate::failure::Cause;
use crate::next::capability::HNil;
use crate::next::kernel::{AnyEnv, Effect, Instr};

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `f` with the current instant on the running fiber's executor clock (virtual time on a
/// `TestExecutor`). `f` is called inside a `Sync` step, so a panic in it becomes `Die`.
pub(super) fn with_now<A, E, R>(
  f: impl FnOnce(Instant) -> Effect<A, E, R> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let now = fiber.executor().now();
    Instr::Sync(Box::new(move |_| f(now).into_instr()))
  })))
}

/// Interrupts the running fiber with its own id (Effect.ts `Effect.interrupt`).
pub(super) fn interrupt_self<A, E, R>() -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(|_, fiber| {
    Instr::Pure(Err(Cause::Interrupt(fiber.id())))
  })))
}

/// Runs an `HNil` effect (a release hook) under any env.
pub(super) fn under_any_env<A, E, R>(eff: Effect<A, E, HNil>) -> Effect<A, E, R>
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

// ── Gate ────────────────────────────────────────────────────────────────────────────────────

struct Waiter {
  id: u64,
  waker: Waker,
  /// A released permit was handed to this waiter; it takes it on its next poll.
  granted: bool,
}

struct GateState {
  available: usize,
  closed: bool,
  next_id: u64,
  waiters: VecDeque<Waiter>,
}

/// A counting gate with FIFO hand-off: a released permit goes to the longest waiter first.
pub(super) struct Gate {
  capacity: usize,
  state: Mutex<GateState>,
}

/// The gate was closed (the pool shut down) before a permit could be taken.
pub(super) struct Closed;

impl Gate {
  pub(super) fn new(capacity: usize) -> Arc<Self> {
    Arc::new(Self {
      capacity,
      state: Mutex::new(GateState {
        available: capacity,
        closed: false,
        next_id: 0,
        waiters: VecDeque::new(),
      }),
    })
  }

  pub(super) fn capacity(&self) -> usize {
    self.capacity
  }

  /// Permits currently held (granted but not yet taken included).
  pub(super) fn in_use(&self) -> usize {
    self.capacity - lock(&self.state).available
  }

  /// Fibers currently waiting for a permit.
  pub(super) fn waiting(&self) -> usize {
    lock(&self.state)
      .waiters
      .iter()
      .filter(|w| !w.granted)
      .count()
  }

  /// Gives one permit back: hands it to the first waiter, or makes it available.
  fn release(&self) {
    let waker = {
      let mut st = lock(&self.state);
      match st.waiters.iter_mut().find(|w| !w.granted) {
        Some(w) => {
          w.granted = true;
          Some(w.waker.clone())
        }
        None => {
          st.available += 1;
          None
        }
      }
    };
    if let Some(w) = waker {
      w.wake();
    }
  }

  /// Closes the gate: every waiter (and every later acquire) gets [`Closed`]. Permits already
  /// held stay valid and are given back as usual.
  pub(super) fn close(&self) {
    let wakers: Vec<Waker> = {
      let mut st = lock(&self.state);
      st.closed = true;
      st.waiters.iter().map(|w| w.waker.clone()).collect()
    };
    for w in wakers {
      w.wake();
    }
  }

  /// Waits for a permit (interruptibly: the effect is a plain future). `None` once the gate is
  /// closed.
  pub(super) fn acquire<E, R>(self: &Arc<Self>) -> Effect<Option<Permit>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let gate = Arc::clone(self);
    crate::next::from_async(move || async move {
      Ok(
        Acquire {
          gate: Some(gate),
          id: None,
        }
        .await
        .ok(),
      )
    })
  }
}

/// One unit of gate capacity; dropping it gives the unit back.
pub(super) struct Permit {
  gate: Arc<Gate>,
}

impl Drop for Permit {
  fn drop(&mut self) {
    self.gate.release();
  }
}

struct Acquire {
  /// `None` once the future has completed.
  gate: Option<Arc<Gate>>,
  /// The waiter registration, once queued.
  id: Option<u64>,
}

impl Future for Acquire {
  type Output = Result<Permit, Closed>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let gate = Arc::clone(
      self
        .gate
        .as_ref()
        .expect("next: Acquire polled after completion"),
    );
    let mut st = lock(&gate.state);
    match self.id {
      None => {
        if st.closed {
          drop(st);
          self.gate = None;
          return Poll::Ready(Err(Closed));
        }
        if st.available > 0 {
          st.available -= 1;
          drop(st);
          self.gate = None;
          return Poll::Ready(Ok(Permit { gate }));
        }
        let id = st.next_id;
        st.next_id += 1;
        st.waiters.push_back(Waiter {
          id,
          waker: cx.waker().clone(),
          granted: false,
        });
        drop(st);
        self.id = Some(id);
        Poll::Pending
      }
      Some(id) => {
        let pos = st
          .waiters
          .iter()
          .position(|w| w.id == id)
          .expect("next: gate waiter registered");
        if st.waiters[pos].granted {
          st.waiters.remove(pos);
          drop(st);
          self.gate = None;
          self.id = None;
          return Poll::Ready(Ok(Permit { gate }));
        }
        if st.closed {
          st.waiters.remove(pos);
          drop(st);
          self.gate = None;
          self.id = None;
          return Poll::Ready(Err(Closed));
        }
        st.waiters[pos].waker.clone_from(cx.waker());
        Poll::Pending
      }
    }
  }
}

impl Drop for Acquire {
  fn drop(&mut self) {
    // Dropped while queued (the fiber was interrupted): leave the queue, and pass on a permit
    // that was already handed over.
    if let (Some(gate), Some(id)) = (self.gate.take(), self.id) {
      let granted = {
        let mut st = lock(&gate.state);
        match st.waiters.iter().position(|w| w.id == id) {
          Some(pos) => st.waiters.remove(pos).is_some_and(|w| w.granted),
          None => false,
        }
      };
      if granted {
        gate.release();
      }
    }
  }
}

// ── Slot ────────────────────────────────────────────────────────────────────────────────────

struct SlotState<T> {
  value: Option<T>,
  next_id: u64,
  wakers: Vec<(u64, Waker)>,
}

/// A one-shot broadcast cell: completed once, read (cloned) by any number of waiters.
pub(super) struct Slot<T> {
  state: Mutex<SlotState<T>>,
}

impl<T: Clone + Send + 'static> Slot<T> {
  pub(super) fn new() -> Arc<Self> {
    Arc::new(Self {
      state: Mutex::new(SlotState {
        value: None,
        next_id: 0,
        wakers: Vec::new(),
      }),
    })
  }

  /// Completes the slot (first completion wins) and wakes every waiter.
  pub(super) fn complete(&self, v: T) {
    let wakers = {
      let mut st = lock(&self.state);
      if st.value.is_some() {
        return;
      }
      st.value = Some(v);
      std::mem::take(&mut st.wakers)
    };
    for (_, w) in wakers {
      w.wake();
    }
  }

  /// Waits (interruptibly) for the slot's value.
  pub(super) fn wait<E, R>(self: &Arc<Self>) -> Effect<T, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let slot = Arc::clone(self);
    crate::next::from_async(move || async move { Ok(SlotWait { slot, id: None }.await) })
  }
}

struct SlotWait<T> {
  slot: Arc<Slot<T>>,
  id: Option<u64>,
}

impl<T: Clone> Future for SlotWait<T> {
  type Output = T;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
    let slot = Arc::clone(&self.slot);
    let mut st = lock(&slot.state);
    if let Some(v) = &st.value {
      return Poll::Ready(v.clone());
    }
    match self.id {
      Some(id) => {
        if let Some((_, w)) = st.wakers.iter_mut().find(|(i, _)| *i == id) {
          w.clone_from(cx.waker());
        }
      }
      None => {
        let id = st.next_id;
        st.next_id += 1;
        st.wakers.push((id, cx.waker().clone()));
        drop(st);
        self.id = Some(id);
      }
    }
    Poll::Pending
  }
}

impl<T> Drop for SlotWait<T> {
  fn drop(&mut self) {
    if let Some(id) = self.id {
      lock(&self.slot.state).wakers.retain(|(i, _)| *i != id);
    }
  }
}
