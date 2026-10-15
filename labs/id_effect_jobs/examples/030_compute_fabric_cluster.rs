//! Ex 123 — Two-node cluster offload when local Fabric saturates.
//!
//! Node A detects memory pressure, builds a [`FabricJobSpec`](id_effect::compute::FabricJobSpec)
//! via [`FabricGovernor::scale_out`](id_effect::compute::FabricGovernor::scale_out),
//! enqueues through `id_effect_jobs`, and a worker node drains the job with its own Fabric.

use id_effect::compute::{
  ComputeFabric, MetricMode, MetricPolicy, RebalanceStrategy, ResourcePolicy, WorkProfile,
};
use id_effect::next::succeed;
use id_effect_jobs::{JobRunner, JobSpec, MemoryJobRunner, drain_jobs};

fn main() {
  // Same per-node caps on both nodes (cluster placement lives in `id_effect_node::placement`).
  let policy = ResourcePolicy {
    memory: MetricPolicy::new(MetricMode::Max { ceiling: 0.85 }),
    cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
    rebalance: RebalanceStrategy::ScaleOut,
  };

  // Node A — saturated; supervisor would choose ScaleOut.
  let node_a = ComputeFabric::with_mock(policy.clone(), 0.95, 0.92);
  node_a.governor().tick();
  let permits_a = node_a.admission().available();
  // The governor throttles a saturated node to its floor (half the permits; 1 when the CPU
  // spread bucket is exhausted), so fewer than the maximum are available.
  assert!(
    permits_a < node_a.admission().max_permits(),
    "saturated node throttles below max permits"
  );

  let job = node_a.governor().scale_out(
    "heavy-batch",
    b"batch-payload".to_vec(),
    WorkProfile::CpuIntensive,
  );
  let runner = MemoryJobRunner::new();
  // A synchronous `main` is a program edge: `block_on` runs the effect on this thread.
  let enqueued = runner
    .enqueue(JobSpec::from_fabric(job))
    .block_on()
    .into_result()
    .expect("enqueue");
  assert_eq!(enqueued.spec.name, "heavy-batch");
  assert!(enqueued.spec.work_profile.is_some());

  // Node B — worker with headroom executes the offloaded job.
  let node_b = ComputeFabric::with_mock(policy, 0.35, 0.55);
  node_b.governor().tick();
  let worker_permits = node_b.admission().available();
  assert!(worker_permits > permits_a);

  let processed = drain_jobs(runner.clone(), 1, |_spec| {
    succeed::<(), id_effect_jobs::JobError, _>(())
  })
  .block_on()
  .into_result()
  .expect("drain");
  println!(
    "worker node permits={worker_permits} (node A had {permits_a}); offloaded jobs={processed}"
  );
  assert_eq!(processed, 1);
  println!("cluster offload complete");
}
