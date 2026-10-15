//! `m-streams` tests for the next kernel's window, join, `state_scan` and transducer operators:
//! the v1 `streaming::{window, join, state_scan, transducer}` unit tests ported, including their
//! bounded-memory (infinite source) cases.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::next::{
  Effect, Stream, Transducer, combine_latest, keyed_join, state_scan, succeed, transducer_filter,
  transducer_map, transducer_take, window_instant,
};
use id_effect::{Cause, Exit};

fn ok<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("effect failed: {c:?}"),
  }
}

fn collect<A: Send + 'static>(s: Stream<A>) -> Vec<A> {
  ok(s.run_collect())
}

/// See `tests/next_stream.rs`: run `pipeline` over an instrumented infinite source and fail if it
/// buffers more than `max_in_flight` source items.
fn assert_bounded_memory<B, F>(pipeline: F, n: usize, max_in_flight: u64) -> Vec<B>
where
  B: Send + 'static,
  F: FnOnce(Stream<u64>) -> Stream<B>,
{
  let produced = Arc::new(AtomicU64::new(0));
  let delivered = Arc::new(AtomicU64::new(0));
  let source = {
    let (produced, delivered) = (Arc::clone(&produced), Arc::clone(&delivered));
    Stream::from_pull(move |want| {
      let start = produced.fetch_add(want as u64, Ordering::SeqCst);
      let end = start + want as u64;
      let in_flight = end - delivered.load(Ordering::SeqCst);
      assert!(
        in_flight <= max_in_flight,
        "{in_flight} source items produced but not yet delivered (bound {max_in_flight}); \
         the pipeline is buffering its upstream"
      );
      succeed(Some((start..end).collect()))
    })
  };
  fn drain<B: Send + 'static>(
    s: Stream<B>,
    mut out: Vec<B>,
    produced: Arc<AtomicU64>,
    delivered: Arc<AtomicU64>,
  ) -> Effect<Vec<B>> {
    s.next_chunk().flat_map(move |next| match next {
      None => succeed(out),
      Some((chunk, rest)) => {
        delivered.store(produced.load(Ordering::SeqCst), Ordering::SeqCst);
        out.extend(chunk.into_vec());
        drain(rest, out, produced, delivered)
      }
    })
  }
  ok(drain(
    pipeline(source).take(n),
    Vec::new(),
    produced,
    delivered,
  ))
}

fn secs(n: u64) -> Instant {
  window_instant(Duration::from_secs(n))
}

fn millis(n: u64) -> Instant {
  window_instant(Duration::from_millis(n))
}

// ── count windows ─────────────────────────────────────────────────────────────────────────────

#[test]
fn tumbling_groups_non_overlapping_chunks() {
  let s: Stream<u8> = Stream::from_iterable([1, 2, 3, 4, 5]);
  assert_eq!(
    collect(s.tumbling(2)),
    vec![vec![1, 2], vec![3, 4], vec![5]]
  );
}

#[test]
fn sliding_emits_overlapping_windows() {
  let s: Stream<u8> = Stream::from_iterable([1, 2, 3, 4]);
  assert_eq!(collect(s.sliding(3, 1)), vec![vec![1, 2, 3], vec![2, 3, 4]]);
}

#[test]
fn sliding_zero_size_yields_empty() {
  let s: Stream<u8> = Stream::from_iterable([1, 2]);
  assert!(collect(s.sliding(0, 1)).is_empty());
  let s: Stream<u8> = Stream::from_iterable([1, 2]);
  assert!(collect(s.sliding(1, 0)).is_empty());
}

// ── time windows ──────────────────────────────────────────────────────────────────────────────

#[test]
fn session_by_gap_splits_on_gap() {
  let items = vec![(secs(0), 1), (secs(1), 2), (secs(10), 3)];
  let s: Stream<(Instant, i32)> = Stream::from_iterable(items);
  let out = collect(s.session_by_gap(Duration::from_secs(5), |(t, _)| *t));
  assert_eq!(
    out,
    vec![vec![(secs(0), 1), (secs(1), 2)], vec![(secs(10), 3)]]
  );
}

#[test]
fn tumbling_by_time_groups_into_buckets() {
  let s: Stream<(Instant, char)> =
    Stream::from_iterable([(secs(0), 'a'), (secs(1), 'b'), (secs(10), 'c')]);
  let out = collect(s.tumbling_by_time(Duration::from_secs(5), |(t, _)| *t));
  assert_eq!(
    out,
    vec![vec![(secs(0), 'a'), (secs(1), 'b')], vec![(secs(10), 'c')]]
  );
}

fn values<T, V>(windows: Vec<Vec<(T, V)>>) -> Vec<Vec<V>> {
  windows
    .into_iter()
    .map(|w| w.into_iter().map(|(_, v)| v).collect())
    .collect()
}

#[test]
fn sliding_by_time_matches_batch_definition_for_ordered_input() {
  // Windows of 3s every 2s over t = 0, 2, 4: [0,3) [2,5) [4,7).
  let s: Stream<(Instant, i32)> = Stream::from_iterable([(secs(0), 1), (secs(2), 2), (secs(4), 3)]);
  let out = collect(s.sliding_by_time(Duration::from_secs(3), Duration::from_secs(2), |(t, _)| *t));
  assert!(out.iter().all(|w| !w.is_empty()));
  assert_eq!(values(out), vec![vec![1, 2], vec![2, 3], vec![3]]);
}

#[test]
fn sliding_by_time_earlier_timestamp_arriving_later_before_first_window_is_kept() {
  let s: Stream<(Instant, char)> = Stream::from_iterable([(millis(10), 'a'), (millis(0), 'b')]);
  let out = collect(s.sliding_by_time(
    Duration::from_millis(5),
    Duration::from_millis(5),
    |(t, _)| *t,
  ));
  assert_eq!(values(out), vec![vec!['b'], vec!['a']]);
}

#[test]
fn sliding_by_time_earlier_timestamp_in_a_later_chunk_before_first_window_is_kept() {
  // Chunks of one item: the window [10,15) is not complete until a later watermark, so the late
  // `t0` item still re-anchors the first window start.
  let s: Stream<Vec<(Instant, char)>> =
    Stream::from_iterable([(millis(10), 'a'), (millis(0), 'b'), (millis(30), 'c')])
      .sliding_by_time(
        Duration::from_millis(5),
        Duration::from_millis(5),
        |(t, _)| *t,
      );
  fn one_at_a_time(
    s: Stream<Vec<(Instant, char)>>,
    mut out: Vec<Vec<char>>,
  ) -> Effect<Vec<Vec<char>>> {
    s.next_chunk_with_size(1).flat_map(move |next| match next {
      None => succeed(out),
      Some((chunk, rest)) => {
        out.extend(values(chunk.into_vec()));
        one_at_a_time(rest, out)
      }
    })
  }
  assert_eq!(
    ok(one_at_a_time(s, Vec::new())),
    vec![vec!['b'], vec!['a'], vec!['c']]
  );
}

// ── windows on infinite sources ───────────────────────────────────────────────────────────────

#[test]
fn tumbling_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.tumbling(2), 3, 64);
  assert_eq!(out, vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
}

#[test]
fn sliding_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.sliding(3, 2), 3, 64);
  assert_eq!(out, vec![vec![0, 1, 2], vec![2, 3, 4], vec![4, 5, 6]]);
}

#[test]
fn session_by_gap_on_infinite_source_is_incremental() {
  // Timestamps 0,1,2 | 10,11,12 | 20,… : sessions of three separated by 8s gaps.
  let out = assert_bounded_memory(
    |s| s.session_by_gap(Duration::from_secs(5), |n| secs(n / 3 * 10 + n % 3)),
    2,
    64,
  );
  assert_eq!(out, vec![vec![0, 1, 2], vec![3, 4, 5]]);
}

#[test]
fn tumbling_by_time_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(
    |s| s.tumbling_by_time(Duration::from_secs(4), |n| secs(*n)),
    2,
    64,
  );
  assert_eq!(out, vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]]);
}

#[test]
fn sliding_by_time_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(
    |s| s.sliding_by_time(Duration::from_secs(4), Duration::from_secs(2), |n| secs(*n)),
    3,
    64,
  );
  assert_eq!(
    out,
    vec![vec![0, 1, 2, 3], vec![2, 3, 4, 5], vec![4, 5, 6, 7]]
  );
}

// ── joins ─────────────────────────────────────────────────────────────────────────────────────

#[test]
fn merge_interleaves_two_streams() {
  let a: Stream<i32> = Stream::from_iterable([1, 2]);
  assert_eq!(
    collect(a.merge(Stream::from_iterable([10, 20]))),
    vec![1, 10, 2, 20]
  );
}

#[test]
fn merge_drains_remaining_when_one_side_finishes_first() {
  let a: Stream<i32> = Stream::from_iterable([1]);
  assert_eq!(
    collect(a.merge(Stream::from_iterable([10, 20, 30]))),
    vec![1, 10, 20, 30]
  );
}

#[test]
fn merge_with_long_finite_side_keeps_every_element() {
  let a: Stream<i32> = Stream::from_iterable([1]);
  let mut expected = vec![1];
  expected.extend(100..300);
  assert_eq!(collect(a.merge(Stream::from_iterable(100..300))), expected);
}

#[test]
fn merge_with_infinite_sides_is_incremental_and_fair() {
  let out = assert_bounded_memory(|s| s.merge(Stream::from_iterable(1000_u64..)), 6, 64);
  assert_eq!(out, vec![0, 1000, 1, 1001, 2, 1002]);
}

#[test]
fn combine_latest_pairs_latest_on_each_update() {
  let left: Stream<i32> = Stream::from_iterable([1, 2]);
  let out = collect(combine_latest(left, Stream::from_iterable(['a', 'b'])));
  assert_eq!(out, vec![(2, 'a'), (2, 'b')]);
}

#[test]
fn combine_latest_with_infinite_sides_is_incremental() {
  let out = assert_bounded_memory(
    |s| combine_latest(s, Stream::from_iterable(['a', 'b'].into_iter().cycle())),
    3,
    64,
  );
  // Under `take(3)` each side yields a 3-item chunk: left 0,1,2 (no right value yet), then
  // right 'a','b','a' each pair with the latest left value 2.
  assert_eq!(out, vec![(2, 'a'), (2, 'b'), (2, 'a')]);
}

#[test]
fn keyed_join_joins_matching_keys() {
  let left: Stream<(&str, i32)> = Stream::from_iterable([("x", 1), ("y", 2)]);
  let out = collect(keyed_join(
    left,
    Stream::from_iterable([("x", 'a'), ("z", 'b')]),
  ));
  assert_eq!(out, vec![("x", 1, 'a')]);
}

#[test]
fn keyed_join_with_infinite_sides_is_incremental() {
  let out = assert_bounded_memory(
    |s| {
      keyed_join(
        s.map(|n| (n % 4, n)),
        Stream::from_iterable((0_u64..).map(|n| (n % 4, n * 10))),
      )
    },
    4,
    64,
  );
  assert_eq!(out, vec![(0, 0, 0), (1, 1, 10), (2, 2, 20), (3, 3, 30)]);
}

#[test]
fn merge_propagates_a_failure_from_either_side() {
  let a: Stream<i32, &str> = Stream::from_iterable([1]);
  let out = a.merge(Stream::fail("right")).run_collect().block_on();
  assert!(matches!(out, Exit::Failure(Cause::Fail("right"))));
}

// ── state_scan ────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum St {
  Idle,
  Active,
}

#[test]
fn state_scan_emits_only_on_transition_to_active() {
  let s: Stream<u8> = Stream::from_iterable([0u8, 1, 1, 0, 1]);
  let out = collect(s.state_scan(St::Idle, |st, x| {
    let next = if x > 0 { St::Active } else { St::Idle };
    let emit = matches!((st, next), (St::Idle, St::Active));
    (next, emit.then_some(x))
  }));
  assert_eq!(out, vec![1, 1]);
}

#[test]
fn state_scan_no_transition_skips_emit() {
  let s: Stream<u8> = Stream::from_iterable([1u8, 1, 1]);
  let out = collect(s.state_scan(0u8, |st, x| if x == st { (st, None) } else { (x, Some(x)) }));
  assert_eq!(out, vec![1]);
}

#[test]
fn state_scan_emits_each_change() {
  let s: Stream<u8> = Stream::from_iterable([1u8, 2, 3]);
  assert_eq!(
    collect(s.state_scan(0u8, |_st, x| (x, Some(x)))),
    vec![1, 2, 3]
  );
}

#[test]
fn free_state_scan_function() {
  let s: Stream<u8> = Stream::from_iterable([1u8, 2]);
  let out = collect(state_scan(s, 0u32, |acc, x| {
    (acc + u32::from(x), Some(acc))
  }));
  assert_eq!(out, vec![0, 1]);
}

#[test]
fn state_scan_on_infinite_source_is_incremental() {
  // Emit the running sum every time it crosses another multiple of 10.
  let out = assert_bounded_memory(
    |s| {
      s.state_scan(0_u64, |sum, x| {
        let next = sum + x;
        (next, (next / 10 > sum / 10).then_some(next))
      })
    },
    3,
    64,
  );
  assert_eq!(out, vec![10, 21, 36]);
}

#[test]
fn state_scan_empty_upstream_yields_empty() {
  let s: Stream<u8> = Stream::from_iterable(std::iter::empty());
  assert!(collect(s.state_scan(0u32, |acc, x| (acc + u32::from(x), Some(acc)))).is_empty());
}

// ── transducers ───────────────────────────────────────────────────────────────────────────────

#[test]
fn map_transducer_doubles_values() {
  let s: Stream<i32> = Stream::from_iterable([1, 2, 3]);
  assert_eq!(
    collect(s.via_transducer(transducer_map(|n: i32| n * 2))),
    vec![2, 4, 6]
  );
}

#[test]
fn filter_transducer_keeps_evens() {
  let s: Stream<i32> = Stream::from_iterable([1, 2, 3, 4]);
  assert_eq!(
    collect(s.transduce_items(transducer_filter(|n: &i32| n % 2 == 0))),
    vec![2, 4]
  );
}

#[test]
fn custom_transducer_step_and_take() {
  // An identity step: the reducer is passed through unchanged.
  let xf: Transducer<i32, Vec<i32>> = Transducer::new(|rf| rf);
  let out = xf.transduce(
    [1, 2],
    Box::new(|mut acc: Vec<i32>, x| {
      acc.push(x);
      acc
    }),
    vec![],
  );
  assert_eq!(out, vec![1, 2]);
  let sum = transducer_take::<i32, i32>(2).transduce([1, 2, 3], Box::new(|acc, n| acc + n), 0);
  assert_eq!(sum, 3);
}

#[test]
fn composed_map_and_filter() {
  let xf = transducer_map(|n: i32| n + 1).compose(transducer_filter(|n: &i32| n % 2 == 0));
  let s: Stream<i32> = Stream::from_iterable([1, 2, 3]);
  assert_eq!(collect(s.via_transducer(xf)), vec![2, 4]);
}

#[test]
fn transducer_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(
    |s| {
      s.via_transducer(
        transducer_map(|n: u64| n * 10).compose(transducer_filter(|n: &u64| n.is_multiple_of(20))),
      )
    },
    3,
    64,
  );
  assert_eq!(out, vec![0, 20, 40]);
}
