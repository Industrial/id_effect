//! Integration tests for Fabric-aware implicit parallelism in core (stream map/filter, sort).

#![cfg(feature = "compute")]

use id_effect::{
  Stream,
  compute::{ComputeFabric, install_fabric},
  run_blocking,
  schema::order,
};
use std::sync::Arc;

fn install_test_fabric() {
  install_fabric(Arc::new(ComputeFabric::memory_cap_max_cpu(1.0)));
}

#[test]
fn parallel_dispatch_integration() {
  install_test_fabric();

  let stream_mapped = run_blocking(
    Stream::from_iterable(0..64_i32)
      .map(|n| n * 2)
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(stream_mapped.len(), 64);

  let stream_filtered = run_blocking(
    Stream::from_iterable(0..64_i32)
      .filter(Box::new(|n: &i32| n % 2 == 0))
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(stream_filtered.len(), 32);

  let ord = order::order::number_i64();
  let data: Vec<i64> = (0..128).rev().collect();
  let sorted = order::order::sort_with(&ord, data.clone());
  assert_eq!(sorted.first(), Some(&0));
  assert_eq!(sorted.last(), Some(&127));
  assert_eq!(order::order::sort_with_serial(&ord, data), sorted);

  let auto_stream = run_blocking(
    Stream::from_iterable(0..2048_i32)
      .map(|n| n + 1)
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(auto_stream.len(), 2048);
}

#[test]
fn large_collection_parallel_paths() {
  install_test_fabric();

  let ord = order::order::number_i64();
  let data: Vec<i64> = (0..3000).rev().collect();
  assert_eq!(
    order::order::sort_with(&ord, data.clone()),
    order::order::sort_with_serial(&ord, data)
  );
}
