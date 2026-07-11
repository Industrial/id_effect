//! Ex 120 — Compute Fabric memory cap with max CPU.
//!
//! Demonstrates admission throttling from telemetry vs policy, then runs a fiber
//! through Fabric on a shared worker pool.

use id_effect::compute::{ComputeFabric, ResourcePolicy};
use id_effect::{ThreadSleepRuntime, run_fork, succeed};

fn main() {
  // Admission demo with mock telemetry
  let mock = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.40, 0.61);
  mock.supervisor().tick();
  let headroom = mock.admission().available();
  mock.set_readings(0.50, 0.90);
  mock.supervisor().tick();
  let pressure = mock.admission().available();
  println!("admission: headroom={headroom} pressure={pressure}");
  assert!(headroom >= pressure);

  // Live fabric + fiber spawn
  let fabric = ComputeFabric::memory_cap_max_cpu(0.85);
  fabric.supervisor().tick();
  let rt = ThreadSleepRuntime::with_fabric(fabric);
  let handle = run_fork(&rt, || (succeed::<u32, (), ()>(42), ()));
  let out = pollster::block_on(handle.join()).expect("join");
  println!("fiber result = {out}");
}
