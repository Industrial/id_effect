//! Integration tests exercising Rayon dispatch paths (`Parallelism::ForceParallel`).

use id_effect::{
  Parallelism, RedBlackTree, Stream, algebra::functor::vec, collections::hash_map, run_blocking,
  schema::order,
};

#[test]
fn parallel_dispatch_integration() {
  let input: Vec<i32> = (0..128).collect();
  let out = vec::map_with(Parallelism::ForceParallel, input.clone(), |x| x + 1);
  assert_eq!(out.len(), 128);
  assert_eq!(out[0], 1);

  let m = hash_map::from_iter([(1_i32, 10), (2, 20), (3, 30)]);
  let mapped = hash_map::map_values_with(Parallelism::ForceParallel, m.clone(), |v| v * 2);
  assert_eq!(hash_map::get(&mapped, &2), Some(&40));
  let filtered = hash_map::filter_with(Parallelism::ForceParallel, &m, |k, _| *k > 1);
  assert!(!hash_map::has(&filtered, &1));

  let mut t = RedBlackTree::empty();
  for i in 0..64_i32 {
    t.insert(i, i * 10);
  }
  assert_eq!(
    t.entries_with(Parallelism::ForceParallel),
    t.entries_serial()
  );
  assert_eq!(t.size_with(Parallelism::ForceParallel), t.size_serial());
  assert_eq!(
    t.greater_than_with(Parallelism::ForceParallel, &32),
    t.greater_than_serial(&32)
  );
  assert_eq!(
    t.less_than_with(Parallelism::ForceParallel, &32),
    t.less_than_serial(&32)
  );

  let stream_mapped = run_blocking(
    Stream::from_iterable(0..64_i32)
      .map_with(Parallelism::ForceParallel, |n| n * 2)
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(stream_mapped.len(), 64);

  let stream_filtered = run_blocking(
    Stream::from_iterable(0..64_i32)
      .filter_with(Parallelism::ForceParallel, Box::new(|n: &i32| n % 2 == 0))
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(stream_filtered.len(), 32);

  let ord = order::order::number_i64();
  let data: Vec<i64> = (0..128).rev().collect();
  let sorted = order::order::sort_with_policy(Parallelism::ForceParallel, &ord, data);
  assert_eq!(sorted.first(), Some(&0));
  assert_eq!(sorted.last(), Some(&127));

  let large: Vec<i32> = (0..2048).collect();
  let large_out = vec::map(large.clone(), |x| x + 1);
  assert_eq!(large_out.len(), 2048);
  assert_eq!(large_out[2047], 2048);

  let auto_stream = run_blocking(
    Stream::from_iterable(0..2048_i32)
      .map(|n| n + 1)
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(auto_stream.len(), 2048);

  let mut small = RedBlackTree::empty();
  for i in 0..32_i32 {
    small.insert(i, i * 2);
  }
  let _ = small.greater_than_serial(&16);
  let _ = small.greater_than(&16);
  let _ = small.greater_than_with(Parallelism::ForceParallel, &16);
  #[allow(deprecated)]
  let _ = small.greater_than_par(&16);
  let _ = small.less_than_serial(&16);
  let _ = small.less_than(&16);
  let _ = small.less_than_with(Parallelism::ForceParallel, &16);
  #[allow(deprecated)]
  let _ = small.less_than_par(&16);
  let _ = small.size();
  let _ = small.size_with(Parallelism::ForceParallel);
  #[allow(deprecated)]
  let _ = small.size_par();
  let _ = small.entries();
  let _ = small.entries_with(Parallelism::ForceParallel);
  #[allow(deprecated)]
  let _ = small.entries_par();
  let _ = small.values();
  let _ = small.values_with(Parallelism::ForceParallel);
  #[allow(deprecated)]
  let _ = small.values_par();
}
