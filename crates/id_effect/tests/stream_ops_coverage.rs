//! Stream combinator coverage for pre-push gate.

use id_effect::Chunk;
use id_effect::streaming::stream::{
  BackpressureDecision, BackpressurePolicy, Stream, backpressure_decision, end_stream,
  merge_time_bucket, send_chunk, stream_from_channel,
};
use id_effect::{Parallelism, run_blocking, succeed};
use std::time::Instant;

#[test]
fn stream_ops_integration() {
  assert_eq!(
    backpressure_decision(BackpressurePolicy::BoundedBlock, 2, 2),
    BackpressureDecision::Block
  );
  assert_eq!(
    backpressure_decision(BackpressurePolicy::DropNewest, 1, 1),
    BackpressureDecision::DropNewest
  );
  assert_eq!(
    backpressure_decision(BackpressurePolicy::DropOldest, 1, 1),
    BackpressureDecision::DropOldest
  );
  assert_eq!(
    backpressure_decision(BackpressurePolicy::Fail, 1, 1),
    BackpressureDecision::Fail
  );
  assert_eq!(
    backpressure_decision(BackpressurePolicy::Fail, 0, 0),
    BackpressureDecision::Enqueue
  );

  let start = Instant::now();
  let map = merge_time_bucket(id_effect::collections::sorted_map::empty(), start, |_| {
    1_i32
  });
  assert_eq!(
    id_effect::collections::sorted_map::get(&map, &start),
    Some(1)
  );

  let s1 = Stream::from_iterable(vec![1_i32, 2, 3]);
  let s2 = Stream::from_iterable(vec![4_i32, 5]);
  let grouped = run_blocking(s1.chain(s2).take(4).grouped(2).run_collect(), ()).unwrap();
  assert_eq!(grouped, vec![vec![1, 2], vec![3, 4]]);

  let scanned = run_blocking(
    Stream::from_iterable(1..=3_i32)
      .scan(0_i32, |acc, x| {
        *acc += x;
        *acc
      })
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(scanned, vec![1, 3, 6]);

  let reduced = run_blocking(
    Stream::from_iterable(vec![1_i32, 2, 3]).run_reduce(|a, b| a + b),
    (),
  )
  .unwrap();
  assert_eq!(reduced, Some(6));

  let filtered = run_blocking(
    Stream::from_iterable(0..10_i32)
      .filter_with(Parallelism::Serial, Box::new(|n: &i32| n % 2 == 0))
      .take_while(Box::new(|n: &i32| *n < 6))
      .drop_while(Box::new(|n: &i32| *n < 2))
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(filtered, vec![2, 4]);

  let mapped = run_blocking(
    Stream::from_iterable(vec![1_i32])
      .map_effect(|n| succeed::<i32, (), ()>(n + 10))
      .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(mapped, vec![11]);

  let (stream, sender) = stream_from_channel::<i32, (), ()>(4);
  run_blocking(send_chunk(&sender, Chunk::singleton(7)), ()).unwrap();
  run_blocking(end_stream(sender), ()).unwrap();
  assert_eq!(run_blocking(stream.run_collect(), ()).unwrap(), vec![7]);

  let unfolded = run_blocking(
    Stream::unfold_effect(0_i32, |n| {
      if n < 3 {
        succeed::<Option<(i32, i32)>, (), ()>(Some((n, n + 1)))
      } else {
        succeed(None)
      }
    })
    .run_collect(),
    (),
  )
  .unwrap();
  assert_eq!(unfolded, vec![0, 1, 2]);

  let fold_sum = run_blocking(
    Stream::from_iterable(vec![1_i32, 2, 3]).run_fold(0_i32, |acc, x| acc + x),
    (),
  )
  .unwrap();
  assert_eq!(fold_sum, 6);

  let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let seen_c = seen.clone();
  run_blocking(
    Stream::from_iterable(vec![1_i32, 2]).run_for_each(move |x| {
      seen_c.lock().unwrap().push(x);
      succeed::<(), (), ()>(())
    }),
    (),
  )
  .unwrap();
  assert_eq!(*seen.lock().unwrap(), vec![1, 2]);

  use id_effect::resource::cache::Cache;
  use std::sync::atomic::{AtomicUsize, Ordering};
  let loads = std::sync::Arc::new(AtomicUsize::new(0));
  let loads_c = loads.clone();
  let cache = run_blocking(
    Cache::make(2, None, move |k: i32| {
      loads_c.fetch_add(1, Ordering::SeqCst);
      succeed::<i32, (), ()>(k * 10)
    }),
    (),
  )
  .unwrap();
  assert_eq!(run_blocking(cache.get(1), ()).unwrap(), 10);
  assert_eq!(run_blocking(cache.get(1), ()).unwrap(), 10);
  assert_eq!(loads.load(Ordering::SeqCst), 1);
  run_blocking(cache.invalidate(1), ()).unwrap();
  assert_eq!(run_blocking(cache.get(1), ()).unwrap(), 10);
  assert_eq!(loads.load(Ordering::SeqCst), 2);
  let stats = run_blocking(cache.stats(), ()).unwrap();
  assert!(stats.hits >= 1);

  let chunk = Chunk::singleton(9_i32);
  assert_eq!(chunk.into_vec(), vec![9]);
}
