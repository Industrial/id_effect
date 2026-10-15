//! `m-streams` tests for producer-fed streams, sinks, channels and fan-out on the 0.7 kernel:
//! the v1 `stream` channel/backpressure/broadcast tests, the v1 `sink` tests, the v1
//! `coordination::channel` stream/sink/fold-variant tests and the v1 `replay` tests, ported.
//!
//! Also the leaf criterion "no spin in `fail`": `StreamSender::fail` on a full queue returns at
//! once and the consumer still reads every chunk sent before it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::{
  BackpressureDecision, BackpressurePolicy, Channel, Chunk, Effect, FoldChannel, HNil, Queue, Sink,
  Stream, StreamChannel, StreamChannelFull, backpressure_decision, broadcast_with_replay,
  end_stream, send_chunk, sleep, stream_from_channel, stream_from_channel_with_policy, succeed,
  sync,
};
use id_effect::{Cause, Exit};

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

fn send<A: Send + 'static, E: Send + 'static>(
  sender: &id_effect::StreamSender<A, E>,
  items: Vec<A>,
) -> Result<(), StreamChannelFull> {
  run(send_chunk(sender, Chunk::from_vec(items)))
}

// ── stream_from_channel ───────────────────────────────────────────────────────────────────────

#[test]
fn stream_from_channel_with_chunks_collects_chunks_in_order() {
  let (stream, sender) = stream_from_channel::<u32, Never, _>(4);
  send(&sender, vec![1, 2]).unwrap();
  send(&sender, vec![3]).unwrap();
  ok(end_stream(sender));
  assert_eq!(ok(stream.run_collect()), vec![1, 2, 3]);
}

#[test]
fn stream_from_channel_with_producer_failure_propagates_failure() {
  let (stream, sender) = stream_from_channel::<u32, &str, _>(4);
  send(&sender, vec![1]).unwrap();
  assert!(sender.fail("producer"));
  assert!(!sender.fail("again"), "a second failure is refused");
  let (chunk, rest) = run(stream.next_chunk())
    .expect("pull")
    .expect("chunk sent before the failure");
  assert_eq!(chunk.into_vec(), vec![1]);
  assert_eq!(run(rest.run_collect()), Err("producer"));
}

#[test]
fn stream_error_propagates_via_fail_after_items() {
  let (stream, sender) = stream_from_channel::<u32, String, _>(8);
  send(&sender, vec![1, 2, 3]).unwrap();
  sender.fail("late".to_owned());
  let seen = Arc::new(Mutex::new(Vec::new()));
  let s2 = Arc::clone(&seen);
  let out = run(stream.run_for_each(move |n| {
    let s2 = Arc::clone(&s2);
    sync(move || s2.lock().unwrap().push(n))
  }));
  assert_eq!(out, Err("late".to_owned()));
  assert_eq!(*seen.lock().unwrap(), vec![1, 2, 3]);
}

#[test]
fn fail_on_a_full_queue_does_not_wait() {
  // The 0.6 `fail` looped `run_blocking(offer)` + `yield_now` until the consumer made room.
  let (stream, sender) = stream_from_channel::<u32, &str, _>(1);
  send(&sender, vec![1]).unwrap();
  let start = Instant::now();
  assert!(sender.fail("full"));
  assert!(
    start.elapsed() < Duration::from_secs(1),
    "fail waited for the consumer"
  );
  let (chunk, rest) = run(stream.next_chunk()).unwrap().unwrap();
  assert_eq!(chunk.into_vec(), vec![1]);
  assert_eq!(run(rest.run_drain()), Err("full"));
}

#[test]
fn dropping_the_last_sender_ends_the_stream() {
  let (stream, sender) = stream_from_channel::<u32, Never, _>(4);
  let clone = sender.clone();
  send(&sender, vec![5]).unwrap();
  drop(sender);
  send(&clone, vec![6]).unwrap();
  drop(clone);
  assert_eq!(ok(stream.run_collect()), vec![5, 6]);
}

#[test]
fn a_producer_fiber_feeds_a_consumer_with_backpressure() {
  let (stream, sender) = stream_from_channel::<u32, Never, _>(2);
  let producer = Stream::<u32, StreamChannelFull>::from_iterable(0..100)
    .run_for_each(move |n| send_chunk(&sender, Chunk::singleton(n)));
  let eff = producer.fork().widen().flat_map(move |h| {
    stream
      .run_collect()
      .widen()
      .flat_map(move |v| h.join().map(move |()| v))
  });
  assert_eq!(run(eff), Ok((0..100).collect::<Vec<_>>()));
}

// ── backpressure ──────────────────────────────────────────────────────────────────────────────

#[test]
fn backpressure_decision_table() {
  use BackpressurePolicy as P;
  assert_eq!(
    backpressure_decision(P::BoundedBlock, 0, 2),
    BackpressureDecision::Enqueue
  );
  assert_eq!(
    backpressure_decision(P::BoundedBlock, 2, 2),
    BackpressureDecision::Block
  );
  assert_eq!(
    backpressure_decision(P::DropNewest, 2, 2),
    BackpressureDecision::DropNewest
  );
  assert_eq!(
    backpressure_decision(P::DropOldest, 2, 2),
    BackpressureDecision::DropOldest
  );
  assert_eq!(
    backpressure_decision(P::Fail, 2, 2),
    BackpressureDecision::Fail
  );
}

#[test]
fn backpressure_decision_with_zero_capacity_treats_capacity_as_one() {
  assert_eq!(
    backpressure_decision(BackpressurePolicy::Fail, 0, 0),
    BackpressureDecision::Enqueue
  );
  assert_eq!(
    backpressure_decision(BackpressurePolicy::Fail, 1, 0),
    BackpressureDecision::Fail
  );
}

#[test]
fn stream_from_channel_backpressure_bounded_blocks() {
  // The second send suspends until the consumer takes the first chunk.
  let (stream, sender) = stream_from_channel::<u32, Never, _>(1);
  send(&sender, vec![1]).unwrap();
  let second_done = Arc::new(AtomicUsize::new(0));
  let flag = Arc::clone(&second_done);
  let sender2 = sender.clone();
  let eff = send_chunk(&sender2, Chunk::singleton(2))
    .flat_map(move |()| sync(move || flag.store(1, Ordering::SeqCst)))
    .fork()
    .widen::<StreamChannelFull>()
    .flat_map(move |h| {
      let done = Arc::clone(&second_done);
      sleep(Duration::from_millis(30))
        .then(sync(move || done.load(Ordering::SeqCst)))
        .flat_map(move |before| {
          stream
            .next_chunk_with_size(1)
            .widen()
            .flat_map(move |first| {
              let (chunk, rest) = first.expect("chunk");
              h.join().map(move |()| (before, chunk.into_vec(), rest))
            })
        })
    });
  let (before, first, rest) = run(eff).expect("send");
  assert_eq!(before, 0, "the send into a full queue must wait");
  assert_eq!(first, vec![1]);
  drop(sender);
  drop(sender2);
  assert_eq!(ok(rest.run_collect()), vec![2]);
}

#[test]
fn stream_from_channel_dropping_discards_newest() {
  let (stream, sender) =
    stream_from_channel_with_policy::<u32, Never, _>(2, BackpressurePolicy::DropNewest);
  for n in 1..=4 {
    send(&sender, vec![n]).unwrap();
  }
  ok(end_stream(sender));
  assert_eq!(ok(stream.run_collect()), vec![1, 2]);
}

#[test]
fn stream_from_channel_sliding_discards_oldest() {
  let (stream, sender) =
    stream_from_channel_with_policy::<u32, Never, _>(2, BackpressurePolicy::DropOldest);
  for n in 1..=4 {
    send(&sender, vec![n]).unwrap();
  }
  ok(end_stream(sender));
  assert_eq!(ok(stream.run_collect()), vec![3, 4]);
}

#[test]
fn send_chunk_fail_policy_returns_err_when_full() {
  let (_stream, sender) =
    stream_from_channel_with_policy::<u32, Never, HNil>(1, BackpressurePolicy::Fail);
  assert_eq!(sender.policy(), BackpressurePolicy::Fail);
  assert_eq!(send(&sender, vec![1]), Ok(()));
  assert_eq!(send(&sender, vec![2]), Err(StreamChannelFull));
  assert_eq!(
    run(sender.send(Chunk::empty())),
    Ok(()),
    "an empty chunk is a no-op"
  );
}

#[test]
fn stream_sender_end_stream_closes_queue() {
  let (stream, sender) = stream_from_channel::<u32, Never, _>(2);
  let keep = sender.clone();
  ok(end_stream(sender));
  // After the end a chunk is discarded; under `BoundedBlock` the send still succeeds.
  assert_eq!(send(&keep, vec![1]), Ok(()));
  assert!(ok(stream.run_collect()).is_empty());
  let (_s, failing) = stream_from_channel::<u32, &str, HNil>(2);
  let keep = failing.clone();
  ok(end_stream(failing));
  assert!(!keep.fail("late"), "fail after the end is refused");
}

#[test]
fn stream_channel_full_display_debug_and_error() {
  assert_eq!(
    StreamChannelFull.to_string(),
    "stream channel is full (backpressure policy Fail)"
  );
  assert_eq!(format!("{StreamChannelFull:?}"), "StreamChannelFull");
  let e: &dyn std::error::Error = &StreamChannelFull;
  assert!(e.source().is_none());
}

// ── Queue / Channel bridges ───────────────────────────────────────────────────────────────────

#[test]
fn from_queue_drains_until_shutdown() {
  let q = Queue::<u32>::unbounded();
  let eff = q
    .offer_all::<Never, _>([1, 2, 3])
    .then(q.shutdown())
    .then(Stream::from_queue(q.clone()).run_collect());
  assert_eq!(ok(eff), vec![1, 2, 3]);
}

#[test]
fn stream_map_filter_collect_via_channel() {
  let ch = Channel::<u32>::duplex_unbounded();
  let eff = ch
    .write::<Never, _>(1)
    .then(ch.write(2))
    .then(ch.write(3))
    .then(ch.shutdown())
    .then(
      ch.to_stream()
        .map(|n| n * 10)
        .filter(|n: &u32| *n != 20)
        .run_collect(),
    );
  assert_eq!(ok(eff), vec![10, 30]);
}

#[test]
fn duplex_channel_to_stream_drains_after_shutdown() {
  let ch = Channel::<u32>::duplex_unbounded().map_in(|s: &str| s.len() as u32);
  let eff = ch
    .write::<Never, _>("ab")
    .then(ch.write("abcd"))
    .then(ch.shutdown())
    .then(Stream::from_channel(&ch).run_collect());
  assert_eq!(ok(eff), vec![2, 4]);
}

#[test]
fn from_channel_reads_only_what_is_pulled() {
  // No shutdown: an eager drain would suspend forever waiting for a fourth item.
  let ch = Channel::<u32>::duplex_unbounded();
  let eff = ch
    .write::<Never, _>(1)
    .then(ch.write(2))
    .then(ch.write(3))
    .then(ch.to_stream().take(3).run_collect());
  assert_eq!(ok(eff), vec![1, 2, 3]);
}

#[test]
fn channel_to_sink_writes_stream_elements_into_channel_queue() {
  let ch = Channel::<u32>::duplex_unbounded();
  let sink = ch.to_sink::<Never, _>();
  let eff = sink
    .run(Stream::from_iterable([4, 5, 6]))
    .then(ch.shutdown())
    .then(ch.to_stream().run_collect());
  assert_eq!(ok(eff), vec![4, 5, 6]);
}

#[test]
fn to_sink_propagates_upstream_failure() {
  let ch = Channel::<u32>::duplex_unbounded();
  let sink = ch.to_sink::<&str, _>();
  let upstream = Stream::from_iterable([1]).chain(Stream::fail("up"));
  assert_eq!(run(sink.run(upstream)), Err("up"));
}

// ── StreamChannel (v1 Channel::from_stream) ───────────────────────────────────────────────────

#[test]
fn stream_channel_reads_elements_then_none() {
  let ch: StreamChannel<u32> = Stream::from_iterable([1, 2]).into_channel();
  let eff = ch.read().zip(ch.read()).zip(ch.read()).zip(ch.read());
  assert_eq!(ok(eff), (((Some(1), Some(2)), None), None));
}

#[test]
fn stream_channel_buffers_multi_element_chunks() {
  let pulls = Arc::new(AtomicUsize::new(0));
  let p = Arc::clone(&pulls);
  let s: Stream<u32> = Stream::from_pull(move |_| {
    let first = p.fetch_add(1, Ordering::SeqCst) == 0;
    succeed(first.then(|| vec![1, 2, 3]))
  });
  let ch = StreamChannel::new(s);
  let eff = ch.read().zip(ch.read()).zip(ch.read());
  assert_eq!(ok(eff), ((Some(1), Some(2)), Some(3)));
  assert_eq!(
    pulls.load(Ordering::SeqCst),
    1,
    "one pull served three reads"
  );
}

#[test]
fn stream_channel_preserves_upstream_error_and_then_ends() {
  let ch: StreamChannel<u32, &str> = Stream::from_iterable([1])
    .chain(Stream::fail("up"))
    .into_channel();
  assert_eq!(run(ch.read()), Ok(Some(1)));
  assert_eq!(run(ch.read()), Err("up"));
  assert_eq!(
    run(ch.read()),
    Ok(None),
    "a failed read ends the stream for every handle"
  );
}

#[test]
fn stream_channel_map_out_and_flat_map_out() {
  let mapped = Stream::<u32>::from_iterable([1, 2])
    .into_channel()
    .map_out(|n| n * 10);
  assert_eq!(ok(mapped.to_stream().run_collect()), vec![10, 20]);
  let expanded = Stream::<u32>::from_iterable([1, 2, 3])
    .into_channel()
    .flat_map_out(|n| if n == 2 { vec![] } else { vec![n, n] });
  assert_eq!(ok(expanded.to_stream().run_collect()), vec![1, 1, 3, 3]);
}

#[test]
fn stream_channel_reads_only_what_is_pulled() {
  let ch: StreamChannel<u64> = Stream::from_iterable(0_u64..).into_channel();
  assert_eq!(ok(ch.to_stream().take(3).run_collect()), vec![0, 1, 2]);
}

#[test]
fn stream_channel_concurrent_reads_take_turns() {
  let ch: StreamChannel<u32> = Stream::from_iterable(0..200).into_channel();
  let reader = |ch: StreamChannel<u32>| ch.to_stream().run_collect();
  let eff = reader(ch.clone())
    .fork()
    .zip(reader(ch.clone()).fork())
    .flat_map(|(a, b)| a.join().zip(b.join()));
  let (mut a, b) = ok(eff);
  a.extend(b);
  a.sort_unstable();
  assert_eq!(a, (0..200).collect::<Vec<_>>());
}

// ── FoldChannel (v1 Channel::from_fold / from_sink) ───────────────────────────────────────────

#[test]
fn fold_channel_accumulates_writes_and_fold_state_returns_current() {
  let ch = FoldChannel::from_fold(0_i32, |acc, x: i32| acc + x);
  let eff = ch
    .write::<Never, _>(1)
    .then(ch.write(2))
    .then(ch.write(3))
    .then(ch.fold_state());
  assert_eq!(ok(eff), 6);
}

#[test]
fn fold_channel_read_returns_snapshots_in_order() {
  let ch = FoldChannel::from_fold(Vec::new(), |mut v: Vec<i32>, x| {
    v.push(x);
    v
  });
  let eff = ch
    .write::<Never, _>(1)
    .then(ch.write(2))
    .then(ch.read())
    .zip(ch.read());
  assert_eq!(ok(eff), (Some(vec![1]), Some(vec![1, 2])));
}

#[test]
fn fold_channel_consume_stream_drains_into_fold_and_returns_final_value() {
  let ch = FoldChannel::from_fold(0_u32, |acc, x: u32| acc + x);
  assert_eq!(ok(ch.consume_stream(Stream::from_iterable(1..=10))), 55);
  assert_eq!(ok(ch.fold_state()), 55);
}

#[test]
fn fold_channel_consume_stream_propagates_upstream_failure() {
  let ch = FoldChannel::from_fold(0_u32, |acc, x: u32| acc + x);
  let upstream = Stream::from_iterable([1, 2]).chain(Stream::fail("up"));
  assert_eq!(run(ch.consume_stream(upstream)), Err("up"));
  assert_eq!(
    ok(ch.fold_state()),
    3,
    "the elements before the failure were folded"
  );
}

#[test]
fn fold_channel_consume_stream_stops_when_interrupted() {
  let ch = FoldChannel::from_fold(0_u64, |acc, _x: u64| acc + 1);
  let slow = Stream::<u64>::from_iterable(0..)
    .map_effect(|n| sleep(Duration::from_millis(1)).then(succeed(n)));
  let eff = ch
    .consume_stream(slow)
    .fork()
    .flat_map(|h| sleep(Duration::from_millis(300)).then(h.interrupt()));
  assert!(matches!(ok(eff), Exit::Failure(Cause::Interrupt(_))));
  // Each pull maps a whole chunk (64 sequential 1 ms effects) before its writes run.
  let folded = ok(ch.fold_state());
  assert!(
    folded > 0 && folded < 10_000 && folded.is_multiple_of(64),
    "folded {folded}"
  );
}

#[test]
fn fold_channel_from_sink_writes_accumulate() {
  let ch = FoldChannel::from_sink(Sink::<u32, u32>::fold_left(0, |a, x| a + x));
  let eff = ch
    .write::<Never, _>(4)
    .then(ch.write(5))
    .then(ch.fold_state());
  assert_eq!(ok(eff), 9);
}

#[test]
#[should_panic(expected = "requires a Sink::fold_left")]
fn fold_channel_from_a_non_fold_sink_panics() {
  let _ = FoldChannel::from_sink(Sink::<Vec<u32>, u32>::collect_all_while(|_| true));
}

#[test]
fn fold_channel_map_out_and_flat_map_out_transform_snapshots() {
  let ch = FoldChannel::from_fold(0_i32, |a, x: i32| a + x)
    .map_out(|s| s * 100)
    .flat_map_out(|s| if s == 100 { vec![] } else { vec![s, -s] });
  let eff = ch
    .write::<Never, _>(1)
    .then(ch.write(1))
    .then(ch.read())
    .zip(ch.read());
  assert_eq!(ok(eff), (Some(200), Some(-200)));
}

#[test]
fn fold_channel_shutdown_ends_reads() {
  let ch = FoldChannel::from_fold(0_i32, |a, x: i32| a + x);
  let eff = ch
    .write::<Never, _>(1)
    .then(ch.shutdown())
    .then(ch.read())
    .zip(ch.read());
  assert_eq!(ok(eff), (Some(1), None));
}

// ── Sink ──────────────────────────────────────────────────────────────────────────────────────

#[test]
fn sink_collect_gathers_all_elements() {
  let out = ok(Sink::collect().run(Stream::from_iterable([1u8, 2, 3])));
  assert_eq!(out, vec![1, 2, 3]);
}

#[test]
fn sink_collect_via_fold_channel_matches_sink_collect() {
  let ch = FoldChannel::from_fold(Vec::new(), |mut v: Vec<u8>, x| {
    v.push(x);
    v
  });
  let via_channel = ok(ch.consume_stream(Stream::from_iterable([1u8, 2, 3])));
  let via_sink = ok(Sink::collect().run(Stream::from_iterable([1u8, 2, 3])));
  assert_eq!(via_channel, via_sink);
}

#[test]
fn sink_drain_discards_all() {
  ok(Sink::drain().run(Stream::from_iterable([1u8, 2, 3])));
}

#[test]
fn sink_collect_while_stops_at_predicate() {
  let pulls = Arc::new(AtomicUsize::new(0));
  let p = Arc::clone(&pulls);
  let s: Stream<u64> = Stream::from_pull(move |n| {
    let start = p.fetch_add(n, Ordering::SeqCst) as u64;
    succeed(Some((start..start + n as u64).collect()))
  });
  let out = ok(Sink::collect_all_while(|x: &u64| *x < 3).run(s));
  assert_eq!(out, vec![0, 1, 2]);
  assert_eq!(
    pulls.load(Ordering::SeqCst),
    64,
    "the infinite source is pulled once"
  );
}

#[test]
fn sink_collect_until_stops_before_matching_element() {
  let out =
    ok(Sink::collect_all_until(|x: &u8| *x == 3).run(Stream::from_iterable([1u8, 2, 3, 4])));
  assert_eq!(out, vec![1, 2]);
}

#[test]
fn sink_zip_runs_two_folds_in_one_pass() {
  let sum = Sink::<u32, u32>::fold_left(0, |a, x| a + x);
  let count = Sink::<usize, u32>::from_fold(0, |a, _| a + 1);
  assert_eq!(
    ok(sum.zip(count).run(Stream::from_iterable([1, 2, 3]))),
    (6, 3)
  );
}

#[test]
#[should_panic(expected = "Sink::zip requires")]
fn sink_zip_of_a_non_fold_sink_panics() {
  let _ = Sink::<Vec<u32>, u32>::collect_all_while(|_| true).zip(Sink::<(), u32>::drain());
}

#[test]
fn sink_to_queue_enqueues_every_element() {
  let q = Queue::<u8>::unbounded();
  let eff = Sink::to_queue(q.clone())
    .run(Stream::from_iterable([1u8, 2, 3]))
    .then(q.take_all::<_>().map_err(|e| panic!("{e}")));
  assert_eq!(ok(eff), vec![1, 2, 3]);
}

#[test]
fn sink_collect_to_map_later_values_win() {
  let out = ok(Sink::collect_to_map().run(Stream::from_iterable([("a", 1), ("b", 2), ("a", 3)])));
  assert_eq!(out, HashMap::from([("a", 3), ("b", 2)]));
}

#[test]
fn sink_for_each_and_stream_run() {
  let seen = Arc::new(AtomicUsize::new(0));
  let s2 = Arc::clone(&seen);
  let sink = Sink::for_each(move |x: usize| {
    let s2 = Arc::clone(&s2);
    sync(move || {
      s2.fetch_add(x, Ordering::SeqCst);
    })
  });
  ok(Stream::from_iterable([1, 2, 3]).run(sink.clone()));
  ok(sink.run(Stream::from_iterable([4])));
  assert_eq!(seen.load(Ordering::SeqCst), 10);
}

#[test]
fn sink_propagates_stream_failure() {
  let s: Stream<u8, &str> = Stream::from_iterable([1]).chain(Stream::fail("x"));
  assert_eq!(run(Sink::collect().run(s)), Err("x"));
}

// ── broadcast / replay ────────────────────────────────────────────────────────────────────────

#[test]
fn broadcast_all_consumers_receive_every_element() {
  let eff = Stream::<u32>::from_iterable(0..50)
    .broadcast(64, 3)
    .flat_map(|(mut streams, pump)| {
      let (c, b, a) = (
        streams.pop().unwrap(),
        streams.pop().unwrap(),
        streams.pop().unwrap(),
      );
      a.run_collect()
        .fork()
        .zip(b.run_collect().fork())
        .zip(c.run_collect().fork())
        .flat_map(move |((a, b), c)| pump.then(a.join().zip(b.join()).zip(c.join())))
    });
  let ((a, b), c) = ok(eff);
  let all: Vec<u32> = (0..50).collect();
  assert_eq!((a, b, c), (all.clone(), all.clone(), all));
}

#[test]
fn broadcast_slow_consumer_does_not_block_fast() {
  // Deterministic form of the v1 test: the pump runs to completion while no branch pulls at
  // all, so a consumer that falls behind never holds the pump (or the other branches) back;
  // each branch keeps its newest `capacity` items.
  let eff = Stream::<u32>::from_iterable(0..200)
    .broadcast(64, 2)
    .flat_map(|(mut streams, pump)| {
      let (slow, fast) = (streams.pop().unwrap(), streams.pop().unwrap());
      pump.then(fast.run_collect().zip(slow.run_collect()))
    });
  let (fast, slow) = ok(eff);
  let newest: Vec<u32> = (136..200).collect();
  assert_eq!(fast, newest);
  assert_eq!(slow, newest);
}

#[test]
fn broadcast_fast_consumer_keeps_up_while_another_lags() {
  let eff = Stream::<u32>::from_iterable(0..200)
    .broadcast(64, 2)
    .flat_map(|(mut streams, pump)| {
      let (slow, fast) = (streams.pop().unwrap(), streams.pop().unwrap());
      let slow = slow.map_effect(|n| sleep(Duration::from_millis(2)).then(succeed(n)));
      fast
        .run_collect()
        .fork()
        .zip(slow.run_collect().fork())
        .flat_map(move |(f, s)| pump.then(f.join().zip(s.join())))
    });
  let (fast, slow) = ok(eff);
  for branch in [&fast, &slow] {
    assert!(
      branch.windows(2).all(|w| w[0] < w[1]),
      "out of order: {branch:?}"
    );
    assert_eq!(
      branch.last(),
      Some(&199),
      "every branch ends with the newest item"
    );
  }
  assert!(slow.len() < 200, "the lagging branch lost its oldest items");
}

#[test]
fn broadcast_upstream_failure_reaches_a_branch_and_the_pump_succeeds() {
  let s: Stream<u32, &str> = Stream::from_iterable([1, 2]).chain(Stream::fail("up"));
  let eff = s.broadcast(8, 1).flat_map(|(mut streams, pump)| {
    let branch = streams.pop().unwrap();
    pump.then(branch.run_collect())
  });
  assert_eq!(run(eff), Err("up"));
}

#[test]
fn replay_fanout_delivers_all_elements() {
  let eff = broadcast_with_replay(Stream::<u32>::from_iterable(vec![1, 2, 3, 4]), 8, 2, 1)
    .flat_map(|(mut streams, pump)| {
      assert_eq!(streams.len(), 1);
      let consumer = streams.remove(0);
      pump.then(consumer.run_collect())
    });
  assert_eq!(ok(eff), vec![1, 2, 3, 4]);
}

#[test]
fn zero_branches_drains_upstream_without_fanout() {
  let pulls = Arc::new(AtomicUsize::new(0));
  let p = Arc::clone(&pulls);
  let s: Stream<u32> = Stream::from_iterable(vec![1, 2]).map_serial(move |n| {
    p.fetch_add(1, Ordering::SeqCst);
    n
  });
  let eff = broadcast_with_replay(s, 4, 1, 0).flat_map(|(streams, pump)| {
    assert!(streams.is_empty());
    pump
  });
  ok(eff);
  assert_eq!(pulls.load(Ordering::SeqCst), 2);
}

#[test]
fn multi_branch_replay_fanout_and_broadcast_replay() {
  let eff = Stream::<u32>::from_iterable(vec![7, 8])
    .broadcast_replay(8, 2)
    .flat_map(|(mut streams, pump)| {
      let (b, a) = (streams.pop().unwrap(), streams.pop().unwrap());
      pump.then(a.run_collect().zip(b.run_collect()))
    });
  assert_eq!(ok(eff), (vec![7, 8], vec![7, 8]));
}
