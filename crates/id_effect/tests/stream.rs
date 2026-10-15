//! `m-streams` tests for `id_effect::Stream`: the v1 `streaming::stream` unit tests ported
//! to the 0.7 kernel (constructors, transformations, consumers, the pull-based / bounded-memory
//! suite and its proptest), plus the leaf's acceptance criteria:
//!
//! - `map_effect` is sequential (one effect in flight, side effects in stream order);
//! - `map_effect_par(n)` is bounded (never more than `n` in flight, and it reaches `n`);
//! - the environment `R` is never cloned by `map_effect` / `map_effect_par`;
//! - a running stream is interrupted like any effect (the v1 cancellation-token tests).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::Never;
use id_effect::{Cause, Exit};
use id_effect::{
  Chunk, DEFAULT_CHUNK_SIZE, Effect, HCons, HNil, Stream, fail, need, sleep, succeed, sync,
  yield_fiber,
};

fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("effect died: {c:?}"),
  }
}

fn ok<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match run(eff) {
    Ok(a) => a,
    Err(n) => match n {},
  }
}

fn collect<A: Send + 'static>(s: Stream<A>) -> Vec<A> {
  ok(s.run_collect())
}

/// Infinite counter source that records the total number of items requested from it.
fn counting_source(requested: Arc<AtomicUsize>) -> Stream<u64> {
  let mut next = 0_u64;
  Stream::from_pull(move |n| {
    requested.fetch_add(n, Ordering::SeqCst);
    let batch: Vec<u64> = (next..next + n as u64).collect();
    next += n as u64;
    succeed(Some(batch))
  })
}

/// Runs `pipeline` over an instrumented infinite source (`0, 1, 2, …`), takes `n` outputs, and
/// asserts the pipeline never holds more than `max_in_flight` source items that were produced
/// but not yet delivered (the v1 `testing::assert_bounded_memory`). An operator that drains its
/// upstream before emitting makes the source panic at the bound instead of hanging.
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
        "assert_bounded_memory: {in_flight} source items produced but not yet delivered \
         (bound {max_in_flight}); the pipeline is buffering its upstream"
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

// ── assert_bounded_memory itself ──────────────────────────────────────────────────────────────

#[test]
fn bounded_memory_identity_pipeline_returns_first_n_naturals() {
  assert_eq!(assert_bounded_memory(|s| s, 3, 64), vec![0, 1, 2]);
}

#[test]
#[should_panic(expected = "buffering its upstream")]
fn bounded_memory_eager_pipeline_exceeds_bound() {
  // `run_collect` on an infinite source drains forever; the source panics at the bound.
  let _ = assert_bounded_memory(|s| Stream::from_effect(s.run_collect()).map(|v| v), 1, 256);
}

// ── constructors ──────────────────────────────────────────────────────────────────────────────

#[test]
fn from_iterable_with_values_collects_values_in_original_order() {
  assert_eq!(collect(Stream::from_iterable([3, 1, 2])), vec![3, 1, 2]);
}

#[test]
fn next_chunk_with_size_returns_incremental_slices_then_none() {
  let s: Stream<i32> = Stream::from_iterable([1, 2, 3, 4, 5]);
  let (c1, s) = ok(s.next_chunk_with_size(2)).expect("first chunk");
  assert_eq!(c1, Chunk::from_vec(vec![1, 2]));
  let (c2, s) = ok(s.next_chunk_with_size(2)).expect("second chunk");
  assert_eq!(c2, Chunk::from_vec(vec![3, 4]));
  let (c3, s) = ok(s.next_chunk_with_size(2)).expect("third chunk");
  assert_eq!(c3, Chunk::from_vec(vec![5]));
  assert!(ok(s.next_chunk_with_size(2)).is_none());
}

#[test]
fn next_chunk_with_zero_size_counts_as_one() {
  // v1 returned `None` without consuming; an owned pull always makes progress instead.
  let s: Stream<i32> = Stream::from_iterable([7, 8]);
  let (c, rest) = ok(s.next_chunk_with_size(0)).expect("chunk");
  assert_eq!(c.into_vec(), vec![7]);
  assert_eq!(collect(rest), vec![8]);
}

#[test]
fn default_chunk_size_is_64() {
  let (c, _) = ok(Stream::<u32>::from_iterable(0..1000).next_chunk()).expect("chunk");
  assert_eq!(c.len(), DEFAULT_CHUNK_SIZE);
  assert_eq!(DEFAULT_CHUNK_SIZE, 64);
}

#[test]
fn range_with_start_and_end_builds_expected_half_open_interval() {
  assert_eq!(collect(Stream::range(2, 6)), vec![2, 3, 4, 5]);
}

#[test]
fn range_empty_when_start_equals_end() {
  assert!(collect(Stream::range(5, 5)).is_empty());
}

#[test]
fn range_empty_when_start_greater_than_end() {
  assert!(collect(Stream::range(9, 5)).is_empty());
}

#[test]
fn unfold_with_generator_stops_when_generator_returns_none() {
  let s: Stream<u32> = Stream::unfold(0_u32, |n| (n < 4).then_some((n, n + 1)));
  assert_eq!(collect(s), vec![0, 1, 2, 3]);
}

#[test]
fn unfold_effect_with_generator_pulls_until_none() {
  let s: Stream<u32> = Stream::unfold_effect(0_u32, |n| {
    succeed(if n < 3 { Some((n * 10, n + 1)) } else { None })
  });
  assert_eq!(collect(s), vec![0, 10, 20]);
}

#[test]
fn succeed_empty_from_vec_and_from_chunk_sources() {
  assert_eq!(collect(Stream::succeed(9)), vec![9]);
  assert!(collect(Stream::<u8>::empty()).is_empty());
  assert_eq!(collect(Stream::from_vec(vec![1, 2])), vec![1, 2]);
  assert_eq!(
    collect(Stream::from_chunk(Chunk::from_vec(vec![3, 4]))),
    vec![3, 4]
  );
}

#[test]
fn fail_source_fails_on_first_pull() {
  assert_eq!(
    run(Stream::<u8, &str>::fail("nope").run_collect()),
    Err("nope")
  );
}

#[test]
fn from_effect_wraps_vec_producing_effect() {
  let s: Stream<u8> = Stream::from_effect(succeed(vec![1, 2, 3]));
  assert_eq!(collect(s), vec![1, 2, 3]);
}

#[test]
fn from_effect_propagates_failure() {
  let s: Stream<u8, &str> = Stream::from_effect(fail("boom"));
  assert_eq!(run(s.run_collect()), Err("boom"));
}

#[test]
fn chain_and_concat_yield_first_then_second() {
  let s: Stream<u8> = Stream::from_iterable([1, 2]).chain(Stream::from_iterable([3]));
  assert_eq!(collect(s), vec![1, 2, 3]);
  let s: Stream<u8> = Stream::empty().concat(Stream::from_iterable([4]));
  assert_eq!(collect(s), vec![4]);
}

#[test]
fn chain_with_unbounded_second_is_incremental() {
  let s: Stream<u64> = Stream::from_iterable([100]).chain(Stream::from_iterable(0..));
  assert_eq!(collect(s.take(4)), vec![100, 0, 1, 2]);
}

#[test]
fn flat_map_concatenates_inner_streams_in_order() {
  let s: Stream<u32> = Stream::from_iterable(1..=3).flat_map(|n| Stream::from_iterable(0..n));
  assert_eq!(collect(s), vec![0, 0, 1, 0, 1, 2]);
  let empty_inners: Stream<u32> = Stream::from_iterable(0..10_000).flat_map(|_| Stream::empty());
  assert!(collect(empty_inners).is_empty());
}

#[test]
fn flat_map_on_infinite_outer_is_incremental() {
  let out = assert_bounded_memory(|s| s.flat_map(|n| Stream::from_iterable([n, n])), 5, 64);
  assert_eq!(out, vec![0, 0, 1, 1, 2]);
}

// ── transformations ───────────────────────────────────────────────────────────────────────────

#[test]
fn stream_throughput_counter_counts_elements() {
  let counter = Arc::new(AtomicU64::new(0));
  let s: Stream<u32> = Stream::from_iterable(0..10).with_throughput_counter(Arc::clone(&counter));
  assert_eq!(collect(s), (0..10).collect::<Vec<_>>());
  assert_eq!(counter.load(Ordering::SeqCst), 10);
}

#[test]
fn map_filter_take_chain_with_values_produces_expected_output() {
  let s: Stream<u32> = Stream::from_iterable(1..=10)
    .map(|n| n * 2)
    .filter(|n: &u32| !n.is_multiple_of(3))
    .take(4);
  assert_eq!(collect(s), vec![2, 4, 8, 10]);
}

#[test]
fn default_map_matches_serial_for_from_iterable() {
  let par: Vec<u64> = collect(Stream::from_iterable(0..10_000).map(|n: u64| n * 7 + 1));
  let ser: Vec<u64> = collect(Stream::from_iterable(0..10_000).map_serial(|n: u64| n * 7 + 1));
  assert_eq!(par, ser);
}

#[test]
fn default_filter_matches_serial_for_from_iterable() {
  let par: Vec<u64> = collect(Stream::from_iterable(0..10_000).filter(|n: &u64| n % 7 == 3));
  let ser: Vec<u64> = collect(Stream::from_iterable(0..10_000).filter_serial(|n: &u64| n % 7 == 3));
  assert_eq!(par, ser);
}

#[test]
fn grouped_with_size_groups_stream_into_fixed_size_batches() {
  let s: Stream<u8> = Stream::from_iterable(1..=5);
  assert_eq!(collect(s.grouped(2)), vec![vec![1, 2], vec![3, 4], vec![5]]);
}

#[test]
fn grouped_with_zero_size_returns_empty_output() {
  let s: Stream<u8> = Stream::from_iterable(1..=5);
  assert!(collect(s.grouped(0)).is_empty());
}

#[test]
fn map_effect_with_mapping_function_lifts_effectful_mapping() {
  let s: Stream<u32> = Stream::from_iterable(1..=3).map_effect(|n| succeed(n * 100));
  assert_eq!(collect(s), vec![100, 200, 300]);
}

#[test]
fn map_effect_with_failures_preserves_upstream_and_mapper_errors() {
  let mapper_fails: Stream<u32, &str> =
    Stream::from_iterable(1..=3).map_effect(|n| if n == 2 { fail("mapper") } else { succeed(n) });
  assert_eq!(run(mapper_fails.run_collect()), Err("mapper"));
  let upstream_fails: Stream<u32, &str> = Stream::<u32, &str>::fail("upstream").map_effect(succeed);
  assert_eq!(run(upstream_fails.run_collect()), Err("upstream"));
}

#[test]
fn pure_transforms_with_incremental_polling_drive_output_from_chunks() {
  let s: Stream<u32> = Stream::from_iterable(0..10)
    .map(|n| n + 1)
    .filter(|n: &u32| n.is_multiple_of(2));
  let (c1, rest) = ok(s.next_chunk_with_size(3)).expect("first");
  assert_eq!(c1.into_vec(), vec![2]);
  let (c2, rest) = ok(rest.next_chunk_with_size(3)).expect("second");
  assert_eq!(c2.into_vec(), vec![4, 6]);
  assert_eq!(collect(rest), vec![8, 10]);
}

#[test]
fn filter_empty_stream_returns_empty() {
  assert!(collect(Stream::<u8>::empty().filter(|_: &u8| true)).is_empty());
}

#[test]
fn take_while_stops_at_first_false() {
  let s: Stream<u8> = Stream::from_iterable([1, 2, 5, 1]);
  assert_eq!(collect(s.take_while(|n: &u8| *n < 3)), vec![1, 2]);
}

#[test]
fn drop_while_skips_initial_run() {
  let s: Stream<u8> = Stream::from_iterable([1, 2, 5, 1]);
  assert_eq!(collect(s.drop_while(|n: &u8| *n < 3)), vec![5, 1]);
}

#[test]
fn scan_emits_running_state_per_element() {
  let s: Stream<u32> = Stream::from_iterable([1, 2, 3]).scan(0, |acc, x| {
    *acc += x;
    *acc
  });
  assert_eq!(collect(s), vec![1, 3, 6]);
}

#[test]
fn scan_with_empty_stream_produces_empty_output() {
  let s: Stream<u32> = Stream::empty().scan(0, |acc: &mut u32, x: u32| {
    *acc += x;
    *acc
  });
  assert!(collect(s).is_empty());
}

#[test]
fn map_err_and_widen_transform_the_error_channel() {
  let s: Stream<u8, String> = Stream::<u8, &str>::fail("e").map_err(|e| format!("{e}!"));
  assert_eq!(run(s.run_collect()), Err("e!".to_owned()));
  let w: Stream<u8, &str> = Stream::<u8>::from_iterable([1]).widen();
  assert_eq!(run(w.run_collect()), Ok(vec![1]));
}

// ── consumers ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn run_fold_with_values_accumulates_all_values() {
  let s: Stream<u32> = Stream::from_iterable(1..=4);
  assert_eq!(ok(s.run_fold(0, |a, b| a + b)), 10);
}

#[test]
fn run_reduce_with_empty_stream_returns_none() {
  assert_eq!(ok(Stream::<u32>::empty().run_reduce(|a, b| a + b)), None);
  assert_eq!(
    ok(Stream::<u32>::from_iterable(1..=4).run_reduce(|a, b| a.max(b))),
    Some(4)
  );
}

#[test]
fn run_for_each_with_callback_executes_effect_for_each_element() {
  let seen = Arc::new(Mutex::new(Vec::new()));
  let s2 = Arc::clone(&seen);
  let s: Stream<u32> = Stream::from_iterable([4, 5, 6]);
  ok(s.run_for_each(move |n| {
    let s2 = Arc::clone(&s2);
    sync(move || s2.lock().unwrap().push(n))
  }));
  assert_eq!(*seen.lock().unwrap(), vec![4, 5, 6]);
}

#[test]
fn run_for_each_effect_collects_side_effects_and_propagates_failure() {
  let seen = Arc::new(AtomicUsize::new(0));
  let s2 = Arc::clone(&seen);
  let s: Stream<u32, &str> = Stream::from_iterable(1..=5);
  let out = run(s.run_for_each_effect(move |n| {
    let s2 = Arc::clone(&s2);
    if n == 3 {
      fail("three")
    } else {
      sync(move || {
        s2.fetch_add(1, Ordering::SeqCst);
      })
    }
  }));
  assert_eq!(out, Err("three"));
  assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[test]
fn run_fold_effect_accumulates_with_effectful_step() {
  let s: Stream<u32> = Stream::from_iterable(1..=4);
  assert_eq!(ok(s.run_fold_effect(1, |acc, x| succeed(acc * x))), 24);
}

#[test]
fn run_fold_effect_propagates_step_failure() {
  let s: Stream<u32, &str> = Stream::from_iterable(1..=4);
  let out = run(s.run_fold_effect(0, |acc, x| {
    if x == 3 {
      fail("step")
    } else {
      succeed(acc + x)
    }
  }));
  assert_eq!(out, Err("step"));
}

#[test]
fn run_drain_pulls_to_the_end() {
  let requested = Arc::new(AtomicUsize::new(0));
  ok(
    counting_source(Arc::clone(&requested))
      .take(130)
      .run_drain(),
  );
  assert_eq!(requested.load(Ordering::SeqCst), 130);
}

#[test]
fn long_streams_run_without_growing_the_stack() {
  // 200k one-element pulls through three stages: the consumer loop is trampolined by the
  // runloop, not recursive.
  let s: Stream<u64> = Stream::from_pull({
    let mut next = 0_u64;
    move |_| {
      next += 1;
      succeed((next <= 200_000).then(|| vec![next]))
    }
  })
  .map(|n| n + 1)
  .filter(|n: &u64| n.is_multiple_of(2))
  .map_effect(succeed);
  assert_eq!(ok(s.run_fold(0_u64, |acc, _| acc + 1)), 100_000);
}

// ── map_effect: sequential (ADR 0008 Amendment 2) ─────────────────────────────────────────────

/// An effect that tracks how many of its kind run at once (`now`/`max`) and logs its start.
fn tracked(
  n: u32,
  now: &Arc<AtomicUsize>,
  max: &Arc<AtomicUsize>,
  log: &Arc<Mutex<Vec<u32>>>,
) -> Effect<u32> {
  let (now_in, now_out, max, log) = (
    Arc::clone(now),
    Arc::clone(now),
    Arc::clone(max),
    Arc::clone(log),
  );
  sync(move || {
    log.lock().unwrap().push(n);
    let cur = now_in.fetch_add(1, Ordering::SeqCst) + 1;
    max.fetch_max(cur, Ordering::SeqCst);
  })
  .then(sleep(Duration::from_millis(10)))
  .then(sync(move || {
    now_out.fetch_sub(1, Ordering::SeqCst);
    n * 10
  }))
}

#[test]
fn map_effect_is_sequential() {
  let (now, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
  let log = Arc::new(Mutex::new(Vec::new()));
  let (n2, m2, l2) = (Arc::clone(&now), Arc::clone(&max), Arc::clone(&log));
  let s: Stream<u32> = Stream::from_iterable(0..20).map_effect(move |n| tracked(n, &n2, &m2, &l2));
  let out = collect(s);
  assert_eq!(out, (0..20).map(|n| n * 10).collect::<Vec<_>>());
  assert_eq!(
    max.load(Ordering::SeqCst),
    1,
    "map_effect must run one effect at a time"
  );
  assert_eq!(
    *log.lock().unwrap(),
    (0..20).collect::<Vec<_>>(),
    "side effects in stream order"
  );
}

#[test]
fn map_effect_calls_f_lazily_one_element_at_a_time() {
  // `f` itself (not only its effect) runs after the previous element's effect completed.
  let done = Arc::new(AtomicUsize::new(0));
  let d2 = Arc::clone(&done);
  let s: Stream<usize> = Stream::from_iterable(0..10_usize).map_effect(move |n| {
    assert_eq!(
      d2.load(Ordering::SeqCst),
      n,
      "f called before the previous effect finished"
    );
    let d3 = Arc::clone(&d2);
    yield_fiber().then(sync(move || d3.fetch_add(1, Ordering::SeqCst)))
  });
  assert_eq!(collect(s), (0..10).collect::<Vec<_>>());
}

#[test]
fn map_effect_failure_ends_the_stream_and_skips_later_elements() {
  let calls = Arc::new(AtomicUsize::new(0));
  let c2 = Arc::clone(&calls);
  let s: Stream<i32, &str> = Stream::from_iterable(0..200).map_effect(move |n: i32| {
    c2.fetch_add(1, Ordering::SeqCst);
    if n == 0 { fail("bad") } else { succeed(n) }
  });
  assert_eq!(run(s.run_collect()), Err("bad"));
  assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn map_effect_is_lazy_until_first_pull() {
  let pulls = Arc::new(AtomicUsize::new(0));
  let p = Arc::clone(&pulls);
  let stream: Stream<i32> = Stream::from_pull(move |_| {
    p.fetch_add(1, Ordering::SeqCst);
    succeed(Some(vec![1]))
  })
  .map_effect(succeed);
  let eff = stream.take(1).run_collect();
  assert_eq!(
    pulls.load(Ordering::SeqCst),
    0,
    "building the stream and its consumer pulls nothing"
  );
  assert_eq!(ok(eff), vec![1]);
  assert_eq!(pulls.load(Ordering::SeqCst), 1);
}

// ── map_effect_par: bounded ───────────────────────────────────────────────────────────────────

#[test]
fn map_effect_par_is_bounded_and_reaches_its_bound() {
  for limit in [1_usize, 3, 8] {
    let (now, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (n2, m2, l2) = (Arc::clone(&now), Arc::clone(&max), Arc::clone(&log));
    let s: Stream<u32> =
      Stream::from_iterable(0..40).map_effect_par(limit, move |n| tracked(n, &n2, &m2, &l2));
    let out = collect(s);
    assert_eq!(
      out,
      (0..40).map(|n| n * 10).collect::<Vec<_>>(),
      "output order"
    );
    let seen = max.load(Ordering::SeqCst);
    assert!(seen <= limit, "limit {limit}: {seen} effects in flight");
    assert_eq!(
      seen, limit,
      "limit {limit}: concurrency never reached the bound"
    );
    assert_eq!(now.load(Ordering::SeqCst), 0);
  }
}

#[test]
fn map_effect_par_zero_counts_as_one() {
  let (now, max) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
  let log = Arc::new(Mutex::new(Vec::new()));
  let (n2, m2, l2) = (Arc::clone(&now), Arc::clone(&max), Arc::clone(&log));
  let s: Stream<u32> =
    Stream::from_iterable(0..10).map_effect_par(0, move |n| tracked(n, &n2, &m2, &l2));
  assert_eq!(collect(s).len(), 10);
  assert_eq!(max.load(Ordering::SeqCst), 1);
}

#[test]
fn map_effect_par_preserves_order_with_uneven_latency() {
  let s: Stream<u64> = Stream::from_iterable(0..32_u64).map_effect_par(6, |n| {
    sleep(Duration::from_millis((32 - n) % 5)).then(succeed(n * 2))
  });
  assert_eq!(collect(s), (0..32).map(|n| n * 2).collect::<Vec<_>>());
}

#[test]
fn map_effect_par_propagates_error_from_inner() {
  let s: Stream<u32, &str> = Stream::from_iterable(0..50).map_effect_par(4, |n| {
    if n == 17 {
      fail("seventeen")
    } else {
      succeed(n)
    }
  });
  assert_eq!(run(s.run_collect()), Err("seventeen"));
}

// ── R is never cloned ─────────────────────────────────────────────────────────────────────────

/// Compile-time half of the "at most 1 R clone per map_effect stream" criterion: these
/// signatures put no `Clone` bound on `R`, so the operators cannot clone the environment.
fn _map_effect_needs_no_clone<R: Send + Sync + 'static>(
  s: Stream<u32, Never, R>,
) -> Stream<u32, Never, R> {
  s.map_effect(succeed).map_effect_par(4, succeed)
}

struct Db(#[allow(dead_code)] u32);

#[test]
fn map_effect_and_map_effect_par_share_the_environment() {
  // The environment is `HCons(Arc<Db>, HNil)`, so cloning it clones the `Arc`. The test holds
  // one reference and the environment another; every effect reads the strong count through a
  // `Weak` (which adds none) while it is in flight. A clone of the environment per element
  // (the v1 `map_effect` cloned `R` for every element of a chunk) would push the count above 2
  // while the concurrent effects of `map_effect_par` sleep.
  let db = Arc::new(Db(1));
  let weak = Arc::downgrade(&db);
  let probe = move |w: std::sync::Weak<Db>| {
    sleep(Duration::from_millis(1)).then(sync(move || w.strong_count()))
  };
  let (w1, w2) = (weak.clone(), weak.clone());
  let s: Stream<usize, Never, HCons<Db, HNil>> = Stream::from_iterable(0..64_u32)
    .map_effect(move |_| probe(w1.clone()))
    .map_effect_par(8, move |c| probe(w2.clone()).map(move |c2| c.max(c2)));
  let with_need = need::<Db>().map(|db: Arc<Db>| Arc::strong_count(&db));
  let counts = ok(
    with_need
      .flat_map(move |during_need| s.run_collect().map(move |v| (during_need, v)))
      .provide_all(HCons(Arc::clone(&db), HNil)),
  );
  assert_eq!(counts.0, 3, "test + environment + the `need` handle");
  assert_eq!(counts.1.len(), 64);
  assert!(
    counts.1.iter().all(|&c| c == 2),
    "strong counts: {:?}",
    counts.1
  );
  drop(db);
}

// ── interruption (the v1 cancellation-token tests) ────────────────────────────────────────────

#[test]
fn an_infinite_stream_of_empty_chunks_is_interruptible() {
  // `filter` rejecting everything: every pull yields an empty chunk. Without a yield between
  // empty chunks this would spin; the fiber is interrupted instead.
  let drain: Effect<(), Never> = Stream::<u64>::from_iterable(0..)
    .filter(|_: &u64| false)
    .run_drain();
  let eff = drain
    .fork()
    .flat_map(|h| sleep(Duration::from_millis(20)).then(h.interrupt()));
  match ok(eff) {
    Exit::Failure(Cause::Interrupt(_)) => {}
    other => panic!("expected an interrupt, got {other:?}"),
  }
}

#[test]
fn a_suspended_map_effect_is_interrupted() {
  let started = Arc::new(AtomicUsize::new(0));
  let s2 = Arc::clone(&started);
  let s: Stream<u32> = Stream::from_iterable(0..).map_effect(move |n| {
    s2.fetch_add(1, Ordering::SeqCst);
    sleep(Duration::from_secs(60)).then(succeed(n))
  });
  let eff = s
    .run_drain()
    .fork()
    .flat_map(|h| sleep(Duration::from_millis(20)).then(h.interrupt()));
  assert!(matches!(ok(eff), Exit::Failure(Cause::Interrupt(_))));
  assert_eq!(started.load(Ordering::SeqCst), 1);
}

// ── pull-based: splitting, fusing, laziness ───────────────────────────────────────────────────

#[test]
fn from_pull_splits_oversized_batches_to_requested_chunk_size() {
  let mut done = false;
  let s: Stream<i32> = Stream::from_pull(move |_| {
    let batch = (!done).then(|| vec![1, 2, 3, 4, 5]);
    done = true;
    succeed(batch)
  });
  let (c1, s) = ok(s.next_chunk_with_size(2)).expect("c1");
  let (c2, s) = ok(s.next_chunk_with_size(2)).expect("c2");
  let (c3, s) = ok(s.next_chunk_with_size(2)).expect("c3");
  assert_eq!(
    (c1.into_vec(), c2.into_vec(), c3.into_vec()),
    (vec![1, 2], vec![3, 4], vec![5])
  );
  assert!(ok(s.next_chunk_with_size(2)).is_none());
}

#[test]
fn from_pull_skips_empty_batches_and_propagates_errors() {
  let mut calls = 0;
  let s: Stream<i32, &str> = Stream::from_pull(move |_| {
    calls += 1;
    match calls {
      1 | 2 => succeed(Some(Vec::new())),
      3 => succeed(Some(vec![7])),
      _ => fail("boom"),
    }
  });
  let (c, s) = run(s.next_chunk()).expect("pull").expect("chunk");
  assert_eq!(c.into_vec(), vec![7]);
  assert_eq!(run(s.next_chunk()).map(|o| o.is_some()), Err("boom"));
}

/// Source failing on its first pull and yielding `[1, 2]` on any later pull, counting pulls.
fn fail_then_items(pulls: Arc<AtomicUsize>) -> Stream<i32, &'static str> {
  Stream::from_pull(move |_| {
    if pulls.fetch_add(1, Ordering::SeqCst) == 0 {
      fail("first")
    } else {
      succeed(Some(vec![1, 2]))
    }
  })
}

#[test]
fn upstream_failure_fuses_pull_stage() {
  let pulls = Arc::new(AtomicUsize::new(0));
  let s = fail_then_items(Arc::clone(&pulls)).map(|n| n * 10);
  assert_eq!(run(s.run_collect()), Err("first"));
  assert_eq!(
    pulls.load(Ordering::SeqCst),
    1,
    "a failed stream is never pulled again"
  );
}

#[test]
fn chain_failure_in_first_fuses_stream() {
  let second_pulls = Arc::new(AtomicUsize::new(0));
  let p = Arc::clone(&second_pulls);
  let second: Stream<i32, &str> = Stream::from_pull(move |_| {
    p.fetch_add(1, Ordering::SeqCst);
    succeed(Some(vec![9]))
  });
  let s = fail_then_items(Arc::new(AtomicUsize::new(0))).chain(second);
  assert_eq!(run(s.run_collect()), Err("first"));
  assert_eq!(second_pulls.load(Ordering::SeqCst), 0);
}

#[test]
fn chain_failure_in_second_fuses_stream() {
  let first: Stream<i32, &str> = Stream::from_effect(succeed(vec![9]));
  let s = first.chain(fail_then_items(Arc::new(AtomicUsize::new(0))));
  let (c, rest) = run(s.next_chunk()).expect("ok").expect("chunk");
  assert_eq!(c.into_vec(), vec![9]);
  assert_eq!(run(rest.next_chunk()).map(|o| o.is_some()), Err("first"));
}

#[test]
fn take_stops_pulling_upstream_once_n_reached() {
  let requested = Arc::new(AtomicUsize::new(0));
  let out = collect(counting_source(Arc::clone(&requested)).take(5));
  assert_eq!(out, vec![0, 1, 2, 3, 4]);
  assert_eq!(
    requested.load(Ordering::SeqCst),
    5,
    "take must not request past n"
  );
}

#[test]
fn take_while_stops_pulling_at_first_false() {
  let requested = Arc::new(AtomicUsize::new(0));
  let out = collect(counting_source(Arc::clone(&requested)).take_while(|n: &u64| *n < 3));
  assert_eq!(out, vec![0, 1, 2]);
  assert_eq!(
    requested.load(Ordering::SeqCst),
    64,
    "exactly one upstream chunk pulled"
  );
}

#[test]
fn map_effect_on_infinite_source_is_incremental() {
  let requested = Arc::new(AtomicUsize::new(0));
  let out = collect(
    counting_source(Arc::clone(&requested))
      .map_effect(|n| succeed(n + 10))
      .take(4),
  );
  assert_eq!(out, vec![10, 11, 12, 13]);
  assert_eq!(requested.load(Ordering::SeqCst), 4);
}

#[test]
fn map_effect_par_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.map_effect_par(4, |n| succeed(n * 2)), 6, 64);
  assert_eq!(out, vec![0, 2, 4, 6, 8, 10]);
}

#[test]
fn from_iterable_over_infinite_iterator_is_lazy() {
  assert_eq!(
    collect(Stream::from_iterable(0_u64..).take(3)),
    vec![0, 1, 2]
  );
}

#[test]
fn unfold_infinite_generator_is_lazy() {
  let s: Stream<u64> = Stream::unfold(1_u64, |s| Some((s, s * 2)));
  assert_eq!(collect(s.take(5)), vec![1, 2, 4, 8, 16]);
}

#[test]
fn unfold_effect_infinite_generator_is_lazy() {
  let s: Stream<u64> = Stream::unfold_effect(0_u64, |s| succeed(Some((s, s + 1))));
  assert_eq!(collect(s.take(4)), vec![0, 1, 2, 3]);
}

#[test]
fn range_with_huge_bound_is_lazy() {
  assert_eq!(collect(Stream::range(0, i64::MAX).take(3)), vec![0, 1, 2]);
}

// ── the 0.6 bounded-memory suite ──────────────────────────────────────────────────────────────

#[test]
fn map_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.map(|n| n * 3), 5, 64);
  assert_eq!(out, vec![0, 3, 6, 9, 12]);
}

#[test]
fn map_serial_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.map_serial(|n| n + 1), 4, 64);
  assert_eq!(out, vec![1, 2, 3, 4]);
}

#[test]
fn filter_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.filter(|n: &u64| n % 2 == 1), 4, 64);
  assert_eq!(out, vec![1, 3, 5, 7]);
}

#[test]
fn filter_serial_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.filter_serial(|n: &u64| n.is_multiple_of(3)), 4, 64);
  assert_eq!(out, vec![0, 3, 6, 9]);
}

#[test]
fn take_while_on_infinite_source_terminates() {
  let out = assert_bounded_memory(|s| s.take_while(|n: &u64| *n < 100), 1000, 64);
  assert_eq!(out, (0..100).collect::<Vec<_>>());
}

#[test]
fn drop_while_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.drop_while(|n: &u64| *n < 70), 3, 128);
  assert_eq!(out, vec![70, 71, 72]);
}

#[test]
fn take_on_infinite_source_terminates() {
  let out = assert_bounded_memory(|s| s.take(200), 1000, 64);
  assert_eq!(out, (0..200).collect::<Vec<_>>());
}

#[test]
fn grouped_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(|s| s.grouped(3), 2, 64);
  assert_eq!(out, vec![vec![0, 1, 2], vec![3, 4, 5]]);
}

#[test]
fn scan_on_infinite_source_is_incremental() {
  let out = assert_bounded_memory(
    |s| {
      s.scan(0_u64, |acc, x| {
        *acc += x;
        *acc
      })
    },
    5,
    64,
  );
  assert_eq!(out, vec![0, 1, 3, 6, 10]);
}

#[test]
fn map_effect_on_infinite_source_stays_bounded() {
  let out = assert_bounded_memory(|s| s.map_effect(|n| succeed(n + 1)), 5, 64);
  assert_eq!(out, vec![1, 2, 3, 4, 5]);
}

#[test]
fn chained_operators_on_infinite_source_stay_bounded() {
  let out = assert_bounded_memory(
    |s| {
      s.map(|n| n * 2)
        .filter(|n: &u64| n.is_multiple_of(3))
        .grouped(2)
        .map(|g| g.iter().sum::<u64>())
    },
    3,
    64,
  );
  assert_eq!(out, vec![6, 30, 54]);
}

mod pull_based_props {
  use super::*;
  use proptest::prelude::*;

  #[derive(Clone, Debug)]
  enum Op {
    Add(u64),
    /// Drop one residue class `x % m == r` (`m >= 5`, so a chain of ≤ 4 filters always keeps
    /// infinitely many elements).
    Reject(u64, u64),
    Take(usize),
  }

  fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
      (0_u64..100).prop_map(Op::Add),
      (5_u64..9)
        .prop_flat_map(|m| (Just(m), 0..m))
        .prop_map(|(m, r)| Op::Reject(m, r)),
      (0_usize..300).prop_map(Op::Take),
    ]
  }

  proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn infinite_source_with_map_filter_take_chain_yields_first_n(
      ops in proptest::collection::vec(op(), 0..=4),
      n in 0_usize..200,
    ) {
      let mut expected: Box<dyn Iterator<Item = u64>> = Box::new(0_u64..);
      for o in ops.clone() {
        expected = match o {
          Op::Add(k) => Box::new(expected.map(move |x| x + k)),
          Op::Reject(m, r) => Box::new(expected.filter(move |x| x % m != r)),
          Op::Take(k) => Box::new(expected.take(k)),
        };
      }
      let expected: Vec<u64> = expected.take(n).collect();

      let out = assert_bounded_memory(
        move |s| {
          ops.into_iter().fold(s, |s, o| match o {
            Op::Add(k) => s.map(move |x| x + k),
            Op::Reject(m, r) => s.filter(move |x: &u64| x % m != r),
            Op::Take(k) => s.take(k),
          })
        },
        n,
        128,
      );
      prop_assert_eq!(out, expected);
    }
  }
}
