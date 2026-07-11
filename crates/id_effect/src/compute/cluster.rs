//! Cluster placement stubs (ScaleOut via jobs/workflow).

use super::policy::{ResourcePolicy, WorkProfile};
use super::supervisor::ComputeSupervisor;
use super::telemetry::TelemetryEngine;

/// How work is placed across cluster nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementMode {
  /// Prefer local Fabric until saturated.
  LocalFirst,
  /// Spread load evenly across workers.
  Spread,
  /// Pin to a named node.
  Affinity {
    /// Target node identifier.
    node: String,
  },
}

/// Cluster-wide and per-node resource rules.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterResourcePolicy {
  /// Cluster aggregate caps.
  pub global: ResourcePolicy,
  /// Node-local caps.
  pub per_node: ResourcePolicy,
  /// Placement strategy when scaling out.
  pub placement: PlacementMode,
}

impl ClusterResourcePolicy {
  /// Local-first placement with identical global and per-node caps.
  pub fn local_first(policy: ResourcePolicy) -> Self {
    Self {
      global: policy.clone(),
      per_node: policy,
      placement: PlacementMode::LocalFirst,
    }
  }
}

/// Serializable job payload produced by [`ComputeSupervisor::scale_out`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FabricJobSpec {
  /// Handler dispatch key.
  pub name: String,
  /// Opaque effect / env bytes.
  pub payload: Vec<u8>,
  /// Work classification for worker placement.
  pub work_profile: WorkProfile,
}

impl<E: TelemetryEngine> ComputeSupervisor<E> {
  /// Build a cluster offload job when local Fabric is saturated.
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
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::{AdmissionController, MockTelemetry};

  #[test]
  fn scale_out_returns_fabric_job_spec() {
    let telemetry = MockTelemetry::new(0.9, 0.9);
    let admission = std::sync::Arc::new(AdmissionController::new(1, 4));
    let policy = ResourcePolicy::memory_cap_max_cpu(0.85);
    let sup = ComputeSupervisor::new(policy, telemetry, admission);
    let job = sup.scale_out(
      "remote-work",
      b"payload".to_vec(),
      WorkProfile::CpuIntensive,
    );
    assert_eq!(job.name, "remote-work");
    assert_eq!(job.work_profile, WorkProfile::CpuIntensive);
  }
}
