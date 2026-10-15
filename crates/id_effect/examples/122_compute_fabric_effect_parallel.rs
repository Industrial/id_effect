//! Ex 122 — Stream parallelism bounded by the Compute Fabric's admission budget.
//!
//! `Stream::map_effect_par` runs element effects on child fibers; under a fabric-backed
//! `Fabric` the number running at once is also capped by its admission permits.

use std::sync::Arc;

use id_effect::compute::{ComputeFabric, ResourcePolicy};
use id_effect::{Fabric, Stream, succeed};

fn main() {
  let fabric = Arc::new(ComputeFabric::with_mock(
    ResourcePolicy::memory_cap_max_cpu(0.85),
    0.30,
    0.50,
  ));
  fabric.governor().tick();
  let budget = fabric.admission().available();
  println!("admission budget = {budget}");

  let stream = Stream::<u32, ()>::from_iterable(0u32..8);
  let mapped = stream.map_effect_par(4, |n| succeed(n * 2));
  let values = Fabric::from_compute(fabric)
    .provide(mapped.run_collect())
    .block_on()
    .into_result()
    .expect("collect");
  println!("mapped = {values:?}");
  assert_eq!(values.len(), 8);
  assert!(values.iter().all(|n| n % 2 == 0));
}
