//! Compute supervisor control loop (monitor → admit → rebalance).

use std::sync::{Arc, RwLock};

use super::adaptive;
use super::admission::AdmissionController;
use super::policy::{MetricMode, ResourcePolicy};
use super::rayon_pool;
use super::spread::CpuSpreadBucket;
use super::telemetry::{TelemetryEngine, TelemetrySnapshot};
use crate::observability::{ComputeEvent, record_compute_event};

#[derive(Debug)]
pub struct ComputeSupervisor<E: TelemetryEngine> {
  policy: ResourcePolicy,
  telemetry: Arc<E>,
  admission: Arc<AdmissionController>,
  last_snapshot: RwLock<TelemetrySnapshot>,
  spread: Option<Arc<CpuSpreadBucket>>,
}

impl<E: TelemetryEngine> ComputeSupervisor<E> {
  pub fn new(
    policy: ResourcePolicy,
    telemetry: Arc<E>,
    admission: Arc<AdmissionController>,
  ) -> Self {
    let snap = telemetry.snapshot();
    let spread = match policy.cpu.mode {
      MetricMode::Spread { per_worker } => Some(Arc::new(CpuSpreadBucket::new(per_worker))),
      _ => None,
    };
    Self {
      policy,
      telemetry,
      admission,
      last_snapshot: RwLock::new(snap),
      spread,
    }
  }

  pub fn policy(&self) -> &ResourcePolicy {
    &self.policy
  }

  pub fn admission(&self) -> &AdmissionController {
    &self.admission
  }

  pub fn snapshot(&self) -> TelemetrySnapshot {
    *self.last_snapshot.read().expect("supervisor snapshot lock")
  }

  pub fn tick(&self) -> TelemetrySnapshot {
    let min_permits = if self.spread.as_ref().is_some_and(|b| !b.try_acquire()) {
      1
    } else {
      self.admission.max_permits().max(1) / 2
    }
    .max(1);
    let snap = self
      .admission
      .rebalance(&*self.telemetry, &self.policy, min_permits);
    *self
      .last_snapshot
      .write()
      .expect("supervisor snapshot lock") = snap;

    adaptive::refresh_adaptive_context();
    let ctx = adaptive::current_adaptive_context();
    rayon_pool::configure_rayon_threads(ctx.rayon_threads);

    record_compute_event(ComputeEvent::SupervisorTick {
      cpu_pct: snap.cpu_pct,
      mem_pct: snap.mem_pct,
      admission_permits: self.admission.available(),
      parallelism_threshold: ctx.parallelism_threshold,
    });

    snap
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::telemetry::MockTelemetry;
  use std::sync::Arc;

  #[test]
  fn tick_updates_snapshot_under_memory_cap() {
    let telemetry = MockTelemetry::new(0.4, 0.61);
    let admission = Arc::new(AdmissionController::new(4, 8));
    let policy = ResourcePolicy::memory_cap_max_cpu(0.85);
    let sup = ComputeSupervisor::new(policy, telemetry, admission);
    let snap = sup.tick();
    assert!(snap.cpu_pct > 0.0);
    assert_eq!(sup.snapshot().mem_pct, snap.mem_pct);
  }

  #[test]
  fn spread_policy_tick_respects_cpu_bucket() {
    let telemetry = MockTelemetry::new(0.1, 0.1);
    let admission = Arc::new(AdmissionController::new(8, 8));
    let policy = ResourcePolicy::unlimited_memory_cpu_spread(0.5);
    let sup = ComputeSupervisor::new(policy, telemetry, admission);
    let _ = sup.tick();
    let snap = sup.tick();
    assert!(snap.cpu_pct >= 0.0);
    assert!(sup.admission().available() >= 1);
  }

  #[test]
  fn accessors_return_live_handles() {
    let telemetry = MockTelemetry::new(0.2, 0.3);
    let admission = Arc::new(AdmissionController::new(2, 4));
    let policy = ResourcePolicy::memory_cap_max_cpu(0.85);
    let sup = ComputeSupervisor::new(policy.clone(), telemetry, admission);
    assert_eq!(sup.policy(), &policy);
    assert_eq!(sup.admission().max_permits(), 4);
  }
}
