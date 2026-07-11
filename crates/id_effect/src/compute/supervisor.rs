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
