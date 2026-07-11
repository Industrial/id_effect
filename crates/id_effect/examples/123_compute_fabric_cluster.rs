//! Ex 123 — Two-node cluster offload when local Fabric saturates.
//!
//! Node A detects memory pressure, builds a [`FabricJobSpec`](id_effect::compute::FabricJobSpec)
//! via [`ComputeSupervisor::scale_out`](id_effect::compute::ComputeSupervisor::scale_out),
//! enqueues through `id_effect_jobs`, and a worker node drains the job with its own Fabric.

use id_effect::compute::{
  ClusterResourcePolicy, ComputeFabric, MetricMode, MetricPolicy, RebalanceStrategy,
  ResourcePolicy, WorkProfile,
};
use id_effect::kernel::succeed;
use id_effect::runtime::run_blocking;
use id_effect_jobs::{JobRunner, JobSpec, MemoryJobRunner, drain_jobs};

fn main() {
  let cluster = ClusterResourcePolicy::local_first(ResourcePolicy {
    memory: MetricPolicy::new(MetricMode::Max { ceiling: 0.85 }),
    cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
    rebalance: RebalanceStrategy::ScaleOut,
  });

  // Node A — saturated; supervisor would choose ScaleOut.
  let node_a = ComputeFabric::with_mock(cluster.global.clone(), 0.95, 0.92);
  node_a.supervisor().tick();
  let permits_a = node_a.admission().available();
  assert_eq!(permits_a, 1, "saturated node throttles to min permits");

  let job = node_a.supervisor().scale_out(
    "heavy-batch",
    b"batch-payload".to_vec(),
    WorkProfile::CpuIntensive,
  );
  let runner = MemoryJobRunner::new();
  let enqueued = run_blocking(runner.enqueue(JobSpec::from_fabric(job)), ()).expect("enqueue");
  assert_eq!(enqueued.spec.name, "heavy-batch");
  assert!(enqueued.spec.work_profile.is_some());

  // Node B — worker with headroom executes the offloaded job.
  let node_b = ComputeFabric::with_mock(cluster.per_node.clone(), 0.35, 0.55);
  node_b.supervisor().tick();
  let worker_permits = node_b.admission().available();
  assert!(worker_permits > permits_a);

  let processed = run_blocking(
    drain_jobs(runner.clone(), 1, |_spec| {
      succeed::<(), id_effect_jobs::JobError, ()>(())
    }),
    (),
  )
  .expect("drain");
  println!(
    "worker node permits={worker_permits} (node A had {permits_a}); offloaded jobs={processed}"
  );
  assert_eq!(processed, 1);
  println!("cluster offload complete");
}
