//! Global admission permits driven by telemetry vs policy.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::policy::ResourcePolicy;
use super::telemetry::{TelemetryEngine, TelemetrySnapshot};

#[derive(Debug)]
pub struct AdmissionController {
  permits: AtomicUsize,
  max_permits: usize,
}

impl AdmissionController {
  pub fn new(initial: usize, max_permits: usize) -> Self {
    Self {
      permits: AtomicUsize::new(initial.min(max_permits)),
      max_permits,
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

  pub fn release(&self) {
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
    snap
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
