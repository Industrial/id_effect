//! Global admission permits driven by telemetry vs policy.
//!
//! Two ways to take a permit: [`AdmissionController::acquire_blocking`] (spins the calling
//! thread; v1 `spawn_admitted` and `spawn_blocking` work only) and the asynchronous
//! [`AdmissionController::admit`], a waker-based future the 0.7 kernel awaits (ADR 0007
//! Amendment 1). Every permit given back — [`release`](AdmissionController::release), a dropped
//! [`AdmissionGuard`], or a [`rebalance`](AdmissionController::rebalance) — wakes the waiting
//! [`Admit`] futures so they retry.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use super::policy::ResourcePolicy;
use super::telemetry::{TelemetryEngine, TelemetrySnapshot};

#[derive(Debug)]
pub struct AdmissionController {
  permits: AtomicUsize,
  max_permits: usize,
  /// Pending [`Admit`] futures by id, woken whenever permits are given back.
  waiters: Mutex<Waiters>,
}

#[derive(Debug, Default)]
struct Waiters {
  next_id: u64,
  queue: VecDeque<(u64, Waker)>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AdmissionController {
  pub fn new(initial: usize, max_permits: usize) -> Self {
    Self {
      permits: AtomicUsize::new(initial.min(max_permits)),
      max_permits,
      waiters: Mutex::new(Waiters::default()),
    }
  }

  /// Asynchronously takes one permit: the future resolves once a permit is free and yields an
  /// [`AdmissionGuard`] that gives it back when dropped. While throttled it is a suspended
  /// future with a registered waker (no thread is held). Dropping the future before it
  /// resolves takes nothing, so a cancelled (interrupted) waiter cannot leak a permit.
  pub fn admit(self: &Arc<Self>) -> Admit {
    self.admit_n(1)
  }

  /// Like [`admit`](Self::admit) for `n` permits taken at once. `n` is clamped to
  /// `1..=max_permits`, so every request can be satisfied eventually.
  pub fn admit_n(self: &Arc<Self>, n: usize) -> Admit {
    Admit {
      ctl: Arc::clone(self),
      n: n.clamp(1, self.max_permits.max(1)),
      id: None,
    }
  }

  /// Number of [`Admit`] futures currently waiting for a permit.
  pub fn waiting(&self) -> usize {
    lock(&self.waiters).queue.len()
  }

  fn try_acquire_n(&self, n: usize) -> bool {
    loop {
      let current = self.permits.load(Ordering::Acquire);
      if current < n {
        return false;
      }
      if self
        .permits
        .compare_exchange_weak(current, current - n, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
      {
        return true;
      }
    }
  }

  /// Wakes every pending [`Admit`]; each retries and the losers stay registered. Waking all
  /// (not one) means a woken waiter that is dropped before it retries cannot swallow the
  /// wake-up another waiter needed.
  fn wake_waiters(&self) {
    let wakers: Vec<Waker> = lock(&self.waiters)
      .queue
      .iter()
      .map(|(_, w)| w.clone())
      .collect();
    for w in wakers {
      w.wake();
    }
  }

  pub fn available(&self) -> usize {
    self.permits.load(Ordering::Acquire)
  }

  pub fn max_permits(&self) -> usize {
    self.max_permits
  }

  pub fn try_acquire(&self) -> bool {
    loop {
      let current = self.permits.load(Ordering::Acquire);
      if current == 0 {
        return false;
      }
      if self
        .permits
        .compare_exchange_weak(current, current - 1, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
      {
        return true;
      }
    }
  }

  /// Block until a permit is available.
  pub fn acquire_blocking(&self) {
    while !self.try_acquire() {
      std::thread::yield_now();
    }
  }

  /// Gives one permit back (capped at `max_permits`) and wakes the waiting [`Admit`] futures.
  pub fn release(&self) {
    self.release_one();
    self.wake_waiters();
  }

  fn release_one(&self) {
    loop {
      let current = self.permits.load(Ordering::Acquire);
      let next = (current + 1).min(self.max_permits);
      if self
        .permits
        .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
      {
        return;
      }
    }
  }

  pub fn rebalance<E: TelemetryEngine>(
    &self,
    engine: &E,
    policy: &ResourcePolicy,
    min_permits: usize,
  ) -> TelemetrySnapshot {
    let snap = engine.snapshot();
    let headroom = snap.min_headroom(policy);
    let desired = if headroom <= 0.0 {
      min_permits
    } else {
      let scaled = (self.max_permits as f32 * headroom / 0.25).ceil() as usize;
      scaled.clamp(min_permits, self.max_permits)
    };
    self.permits.store(desired, Ordering::Release);
    self.wake_waiters();
    snap
  }
}

/// The future returned by [`AdmissionController::admit`]: resolves to an [`AdmissionGuard`]
/// once the permits are taken.
///
/// The permits are taken inside the very `poll` that returns `Ready`, so there is no state in
/// which this future owns a permit: dropping it while pending (an interrupted waiter) only
/// deregisters its waker.
#[derive(Debug)]
#[must_use = "futures do nothing unless polled"]
pub struct Admit {
  ctl: Arc<AdmissionController>,
  n: usize,
  /// This waiter's id in the controller's waiter list, once registered.
  id: Option<u64>,
}

impl Admit {
  fn deregister(&mut self) {
    if let Some(id) = self.id.take() {
      lock(&self.ctl.waiters).queue.retain(|(w, _)| *w != id);
    }
  }

  fn granted(&mut self) -> AdmissionGuard {
    self.deregister();
    AdmissionGuard {
      ctl: Arc::clone(&self.ctl),
      n: self.n,
    }
  }
}

impl Future for Admit {
  type Output = AdmissionGuard;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AdmissionGuard> {
    let this = self.get_mut();
    if this.ctl.try_acquire_n(this.n) {
      return Poll::Ready(this.granted());
    }
    {
      let mut ws = lock(&this.ctl.waiters);
      let ws = &mut *ws;
      match this.id {
        Some(id) => {
          if let Some((_, w)) = ws.queue.iter_mut().find(|(w, _)| *w == id) {
            w.clone_from(cx.waker());
          }
        }
        None => {
          let id = ws.next_id;
          ws.next_id += 1;
          ws.queue.push_back((id, cx.waker().clone()));
          this.id = Some(id);
        }
      }
    }
    // Re-check after registering: a permit given back between the first attempt and the
    // registration woke nobody.
    if this.ctl.try_acquire_n(this.n) {
      return Poll::Ready(this.granted());
    }
    Poll::Pending
  }
}

impl Drop for Admit {
  fn drop(&mut self) {
    self.deregister();
  }
}

/// Permits taken by an [`Admit`]; dropping the guard gives them back (and wakes the waiters).
#[derive(Debug)]
#[must_use = "dropping the guard releases the permit at once"]
pub struct AdmissionGuard {
  ctl: Arc<AdmissionController>,
  n: usize,
}

impl AdmissionGuard {
  /// How many permits this guard holds.
  pub fn permits(&self) -> usize {
    self.n
  }
}

impl Drop for AdmissionGuard {
  fn drop(&mut self) {
    for _ in 0..self.n {
      self.ctl.release_one();
    }
    self.ctl.wake_waiters();
  }
}

impl Default for AdmissionController {
  fn default() -> Self {
    let max = std::thread::available_parallelism()
      .map(|n| n.get())
      .unwrap_or(4)
      .max(1);
    Self::new(max, max)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::policy::{MetricMode, MetricPolicy, RebalanceStrategy};
  use crate::compute::telemetry::MockTelemetry;

  #[test]
  fn throttles_when_memory_over_ceiling() {
    let engine = MockTelemetry::new(0.5, 0.90);
    let policy = crate::compute::policy::ResourcePolicy {
      memory: MetricPolicy::new(MetricMode::Max { ceiling: 0.85 }),
      cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
      rebalance: RebalanceStrategy::ThrottleAdmission,
    };
    let admission = AdmissionController::new(8, 8);
    admission.rebalance(&*engine, &policy, 1);
    assert_eq!(admission.available(), 1);
  }

  #[test]
  fn admits_more_under_headroom() {
    let engine = MockTelemetry::new(0.4, 0.61);
    let policy = crate::compute::policy::ResourcePolicy::memory_cap_max_cpu(0.85);
    let admission = AdmissionController::new(1, 8);
    admission.rebalance(&*engine, &policy, 1);
    assert!(admission.available() >= 2);
  }

  #[test]
  fn try_acquire_fails_when_empty() {
    let admission = AdmissionController::new(0, 4);
    assert!(!admission.try_acquire());
  }

  #[test]
  fn release_caps_at_max_permits() {
    let admission = AdmissionController::new(4, 4);
    admission.release();
    assert_eq!(admission.available(), 4);
  }

  #[test]
  fn acquire_blocking_obtains_permit() {
    let admission = AdmissionController::new(0, 2);
    admission.release();
    admission.acquire_blocking();
    assert_eq!(admission.available(), 0);
  }

  #[test]
  fn default_matches_host_parallelism() {
    let admission = AdmissionController::default();
    assert!(admission.max_permits() >= 1);
    assert!(admission.available() >= 1);
  }
}
