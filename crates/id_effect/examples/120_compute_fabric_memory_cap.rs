//! Ex 120 — Compute Fabric memory cap with max CPU.
//!
//! Demonstrates admission throttling from telemetry vs policy, then runs fibers admitted
//! through the fabric (`Fabric::from_compute` + `Concurrency::Inherit`).

use std::sync::Arc;

use id_effect::compute::{ComputeFabric, ResourcePolicy};
use id_effect::next::{Concurrency, Effect, Fabric, all, succeed};

fn main() {
  // Admission demo with mock telemetry
  let mock = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.40, 0.61);
  mock.governor().tick();
  let headroom = mock.admission().available();
  mock.set_readings(0.50, 0.90);
  mock.governor().tick();
  let pressure = mock.admission().available();
  println!("admission: headroom={headroom} pressure={pressure}");
  assert!(headroom >= pressure);

  // Live fabric: `Inherit` children await one of its admission permits.
  let fabric = Arc::new(ComputeFabric::memory_cap_max_cpu(0.85));
  fabric.governor().tick();
  let work: Vec<Effect<u32, ()>> = (0..4).map(|_| succeed(42)).collect();
  let out = Fabric::from_compute(fabric)
    .provide(all(work, Concurrency::Inherit))
    .block_on()
    .into_result()
    .expect("admitted fibers");
  println!("fiber results = {out:?}");
  assert_eq!(out, vec![42; 4]);
}
