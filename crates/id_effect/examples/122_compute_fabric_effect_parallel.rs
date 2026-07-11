//! Ex 122 — Adaptive stream parallelism via Compute Fabric admission budget.
//!
//! `Stream::map_effect` caps concurrent effect mappers from the supervisor snapshot.

use id_effect::compute::{ComputeFabric, ResourcePolicy, install_fabric};
use id_effect::streaming::Stream;
use id_effect::{runtime::run_blocking, succeed};
use std::sync::Arc;

fn main() {
  let fabric = Arc::new(ComputeFabric::with_mock(
    ResourcePolicy::memory_cap_max_cpu(0.85),
    0.30,
    0.50,
  ));
  install_fabric(Arc::clone(&fabric));
  fabric.supervisor().tick();
  let budget = fabric.admission().available();
  println!("admission budget = {budget}");

  let stream = Stream::from_iterable((0u32..8).collect::<Vec<_>>());
  let mapped = stream.map_effect(|n| succeed::<u32, (), ()>(n * 2));
  let values = run_blocking(mapped.run_collect(), ()).expect("collect");
  println!("mapped = {values:?}");
  assert_eq!(values.len(), 8);
  assert!(values.iter().all(|n| n % 2 == 0));
}
