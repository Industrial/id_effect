//! Fabric governor control loop (monitor → admit → rebalance).
//!
//! This is *resource governance* (admission, thread sizing, scale-out), not fault supervision;
//! restart policies live in the `Supervisor`.

use std::sync::{Arc, RwLock};

use super::adaptive;
use super::admission::AdmissionController;
use super::policy::{MetricMode, ResourcePolicy, WorkProfile};
use super::rayon_pool;
use super::spread::CpuSpreadBucket;
use super::telemetry::{TelemetryEngine, TelemetrySnapshot};
use crate::next::observability::{ComputeEvent, record_compute_event};

/// Resource governor for a [`super::ComputeFabric`]: compares telemetry to [`ResourcePolicy`]
/// and throttles admission, resizes Rayon, or emits scale-out jobs.
#[derive(Debug)]
pub struct FabricGovernor<E: TelemetryEngine> {
  policy: ResourcePolicy,
  telemetry: Arc<E>,
  admission: Arc<AdmissionController>,
  last_snapshot: RwLock<Option<TelemetrySnapshot>>,
  spread: Option<Arc<CpuSpreadBucket>>,
}

impl<E: TelemetryEngine> FabricGovernor<E> {
  /// Build a governor. Does not sample telemetry; the first [`Self::snapshot`] or
  /// [`Self::tick`] does.
  pub fn new(
    policy: ResourcePolicy,
    telemetry: Arc<E>,
    admission: Arc<AdmissionController>,
  ) -> Self {
    let spread = match policy.cpu.mode {
      MetricMode::Spread { per_worker } => Some(Arc::new(CpuSpreadBucket::new(per_worker))),
      _ => None,
    };
    Self {
      policy,
      telemetry,
      admission,
      last_snapshot: RwLock::new(None),
      spread,
    }
  }

  pub fn policy(&self) -> &ResourcePolicy {
    &self.policy
  }

  pub fn admission(&self) -> &AdmissionController {
    &self.admission
  }

  /// Last telemetry snapshot; samples telemetry once if no tick has run yet.
  pub fn snapshot(&self) -> TelemetrySnapshot {
    if let Some(snap) = *self.last_snapshot.read().expect("governor snapshot lock") {
      return snap;
    }
    let mut slot = self.last_snapshot.write().expect("governor snapshot lock");
    *slot.get_or_insert_with(|| self.telemetry.snapshot())
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
    *self.last_snapshot.write().expect("governor snapshot lock") = Some(snap);

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

  /// Build a cluster offload job when local Fabric is saturated.
  ///
  /// The job carries no target node; cluster placement (choosing a node from mesh load reports)
  /// lives in `id_effect_node::placement`.
  pub fn scale_out(
    &self,
    name: impl Into<String>,
    payload: Vec<u8>,
    profile: WorkProfile,
  ) -> FabricJobSpec {
    FabricJobSpec {
      name: name.into(),
      payload,
      work_profile: profile,
      target_node: None,
    }
  }
}

/// Serializable job payload produced by [`FabricGovernor::scale_out`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricJobSpec {
  /// Handler dispatch key.
  pub name: String,
  /// Opaque effect / env bytes.
  pub payload: Vec<u8>,
  /// Work classification for worker placement.
  pub work_profile: WorkProfile,
  /// Optional target node from placement scoring.
  pub target_node: Option<String>,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::telemetry::MockTelemetry;
  use std::sync::Arc;

  #[test]
  fn scale_out_returns_fabric_job_spec() {
    let telemetry = MockTelemetry::new(0.9, 0.9);
    let admission = Arc::new(AdmissionController::new(1, 4));
    let policy = ResourcePolicy::memory_cap_max_cpu(0.85);
    let sup = FabricGovernor::new(policy, telemetry, admission);
    let job = sup.scale_out(
      "remote-work",
      b"payload".to_vec(),
      WorkProfile::CpuIntensive,
    );
    assert_eq!(job.name, "remote-work");
    assert_eq!(job.work_profile, WorkProfile::CpuIntensive);
    assert_eq!(job.payload, b"payload");
    assert!(job.target_node.is_none());
  }

  #[test]
  fn tick_updates_snapshot_under_memory_cap() {
    let telemetry = MockTelemetry::new(0.4, 0.61);
    let admission = Arc::new(AdmissionController::new(4, 8));
    let policy = ResourcePolicy::memory_cap_max_cpu(0.85);
    let sup = FabricGovernor::new(policy, telemetry, admission);
    let snap = sup.tick();
    assert!(snap.cpu_pct > 0.0);
    assert_eq!(sup.snapshot().mem_pct, snap.mem_pct);
  }

  #[test]
  fn spread_policy_tick_respects_cpu_bucket() {
    let telemetry = MockTelemetry::new(0.1, 0.1);
    let admission = Arc::new(AdmissionController::new(8, 8));
    let policy = ResourcePolicy::unlimited_memory_cpu_spread(0.5);
    let sup = FabricGovernor::new(policy, telemetry, admission);
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
    let sup = FabricGovernor::new(policy.clone(), telemetry, admission);
    assert_eq!(sup.policy(), &policy);
    assert_eq!(sup.admission().max_permits(), 4);
  }
}
