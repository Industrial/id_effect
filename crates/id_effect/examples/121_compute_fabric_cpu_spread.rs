//! Ex 121 — Compute Fabric CPU spread mode with token bucket.
//!
//! Demonstrates even per-worker CPU share under `MetricMode::Spread` and adaptive admission.

use id_effect::compute::{ComputeFabric, CpuSpreadBucket, ResourcePolicy, WorkProfile};
use id_effect::observability::{TracingConfig, install_tracing_layer, snapshot_tracing};
use id_effect::runtime::run_blocking;

fn main() {
  let spread_policy = ResourcePolicy::unlimited_memory_cpu_spread(0.25);
  let fabric = ComputeFabric::with_mock(spread_policy, 0.55, 0.40);
  let _ = run_blocking(install_tracing_layer(TracingConfig::enabled()), ());

  let bucket = CpuSpreadBucket::new(0.25);
  assert!(bucket.try_acquire());
  assert!(bucket.try_acquire() || bucket.available() < 0.25);

  fabric.supervisor().tick();
  let permits = fabric.admission().available();
  println!("spread mode admission permits = {permits}");

  let job =
    fabric
      .supervisor()
      .scale_out("spread-work", b"step".to_vec(), WorkProfile::CpuIntensive);
  println!(
    "scale-out job = {} profile {:?}",
    job.name, job.work_profile
  );

  let snap = snapshot_tracing();
  assert!(!snap.compute_events.is_empty());
  println!("compute events = {}", snap.compute_events.len());
}
