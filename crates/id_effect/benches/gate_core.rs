//! 0.7 gate benches for the core crate (release plan W0, `b0-bench-core`).
//!
//! Every benchmark ID here is compared against the `bench/v0.6` baseline by
//! `scripts/bench-compare.sh` using the thresholds in `docs/benchmarks/gates.toml`. Keep the IDs
//! stable; change the bodies only when an API migration forces it. Run:
//!
//! ```bash
//! cargo bench -p id_effect --bench gate_core
//! ```
//!
//! Kernel measured, per bench:
//!
//! - `gate/async_bind_64`, `gate/flat_map_depth_10k`: the 0.7 kernel (spec
//!   k-rebuild §7 Q-E), run through its real edge `Effect::block_on` (v0.6 ran
//!   `pollster::block_on(effect.run(&mut ()))`). Same workloads: 64 binds through `from_async`,
//!   and a left-nested 10k-deep `succeed` bind chain (build plus run).
//! - `stream/*`: the 0.7 `Stream` (`m-streams`), run through `Effect::block_on` (v0.6 ran
//!   `pollster::block_on(effect.run(&mut ()))`). Same workloads; `map_effect` is now sequential
//!   (ADR 0008 Amendment 2), where v0.6 ran each chunk concurrently under the admission budget.
//! - `coord/*`: the 0.7 `Channel` / `Queue` (`c-coordination`): a forked producer
//!   fiber and the main fiber as consumer, run through `Effect::block_on` (v0.6 ran two
//!   futures under `tokio::join!` on a current-thread runtime). Same workloads; the bounded
//!   queue's `offer` now suspends while full instead of the producer polling `yield_now`.

use ::id_effect::{self as k, HNil};
use criterion::{Criterion, criterion_group, criterion_main};

const ASYNC_BINDS: usize = 64;
const FLAT_MAP_DEPTH: usize = 10_000;
const STREAM_PULL_LEN: u64 = 10_000;
const STREAM_MAP_EFFECT_LEN: u64 = 1_000;
const COORD_MESSAGES: u64 = 10_000;
const QUEUE_CAPACITY: usize = 64;

/// 64 sequential binds, each through a real `async` step (`from_async`).
fn bench_async_bind_64(c: &mut Criterion) {
  c.bench_function("gate/async_bind_64", |b| {
    b.iter(|| {
      let mut effect: k::Effect<usize, (), HNil> = k::succeed(0);
      for _ in 0..ASYNC_BINDS {
        effect = effect.flat_map(|n| k::from_async(move || async move { Ok(n + 1) }));
      }
      let out = effect
        .block_on()
        .into_result()
        .expect("async bind chain should not fail");
      std::hint::black_box(out)
    });
  });
}

/// A left-nested `flat_map` chain 10k deep (build plus run).
fn bench_flat_map_depth_10k(c: &mut Criterion) {
  c.bench_function("gate/flat_map_depth_10k", |b| {
    b.iter(|| {
      let mut effect: k::Effect<usize, (), HNil> = k::succeed(0);
      for _ in 0..FLAT_MAP_DEPTH {
        effect = effect.flat_map(|n| k::succeed(n + 1));
      }
      let out = effect
        .block_on()
        .into_result()
        .expect("deep flat_map should not fail");
      std::hint::black_box(out)
    });
  });
}

/// Pull 10k elements through a pure `map` stage into a collected vector.
fn bench_stream_pull_map_10k(c: &mut Criterion) {
  c.bench_function("stream/pull_map_10k", |b| {
    b.iter(|| {
      let effect = k::Stream::<u64, (), HNil>::from_iterable(0..STREAM_PULL_LEN)
        .map(|n| n.wrapping_mul(3))
        .run_collect();
      let out: Vec<u64> = effect
        .block_on()
        .into_result()
        .expect("stream pull should not fail");
      std::hint::black_box(out.len())
    });
  });
}

/// Run an effect per element for 1k elements via `map_effect`.
fn bench_stream_map_effect_1k(c: &mut Criterion) {
  c.bench_function("stream/map_effect_1k", |b| {
    b.iter(|| {
      let effect = k::Stream::<u64, (), HNil>::from_iterable(0..STREAM_MAP_EFFECT_LEN)
        .map_effect(|n| k::succeed(n.wrapping_mul(3)))
        .run_collect();
      let out: Vec<u64> = effect
        .block_on()
        .into_result()
        .expect("stream map_effect should not fail");
      std::hint::black_box(out.len())
    });
  });
}

/// Writes `n..COORD_MESSAGES` into `channel`, one `write` per element.
fn produce(channel: k::Channel<u64>, n: u64) -> k::Effect<(), (), HNil> {
  if n == COORD_MESSAGES {
    return k::succeed(());
  }
  channel.write(n).flat_map(move |()| produce(channel, n + 1))
}

/// Reads `left` elements from `channel`, summing them.
fn consume(channel: k::Channel<u64>, left: u64, sum: u64) -> k::Effect<u64, (), HNil> {
  if left == 0 {
    return k::succeed(sum);
  }
  channel.read().flat_map(move |n| {
    let n = n.expect("channel should not end early");
    consume(channel, left - 1, sum.wrapping_add(n))
  })
}

/// One producer fiber writes 10k elements into a queue-backed `Channel`; the main fiber reads
/// them back.
fn bench_channel_spsc_10k(c: &mut Criterion) {
  c.bench_function("coord/channel_spsc_10k", |b| {
    b.iter(|| {
      let channel = k::Channel::<u64>::duplex_unbounded();
      let program = produce(channel.clone(), 0)
        .fork()
        .widen()
        .flat_map(move |_producer| consume(channel, COORD_MESSAGES, 0));
      let sum = program
        .block_on()
        .into_result()
        .expect("channel spsc should not fail");
      std::hint::black_box(sum)
    });
  });
}

/// Offers `n..COORD_MESSAGES` into `queue` (each offer suspends while the queue is full).
fn offer_all(queue: k::Queue<u64>, n: u64) -> k::Effect<(), k::QueueError, HNil> {
  if n == COORD_MESSAGES {
    return k::succeed(());
  }
  queue
    .offer(n)
    .flat_map(move |_stored| offer_all(queue, n + 1))
}

/// Takes `left` elements from `queue`, summing them.
fn take_all(queue: k::Queue<u64>, left: u64, sum: u64) -> k::Effect<u64, k::QueueError, HNil> {
  if left == 0 {
    return k::succeed(sum);
  }
  queue
    .take()
    .flat_map(move |n| take_all(queue, left - 1, sum.wrapping_add(n)))
}

/// One producer fiber offers 10k elements into a bounded queue (capacity 64, suspending while
/// full); the main fiber takes them.
fn bench_queue_bounded_10k(c: &mut Criterion) {
  c.bench_function("coord/queue_bounded_10k", |b| {
    b.iter(|| {
      let queue = k::Queue::<u64>::bounded(QUEUE_CAPACITY);
      let program = offer_all(queue.clone(), 0)
        .fork()
        .widen()
        .flat_map(move |_producer| take_all(queue, COORD_MESSAGES, 0));
      let sum = program
        .block_on()
        .into_result()
        .expect("bounded queue should not fail");
      std::hint::black_box(sum)
    });
  });
}

criterion_group!(
  gate_core,
  bench_async_bind_64,
  bench_flat_map_depth_10k,
  bench_stream_pull_map_10k,
  bench_stream_map_effect_1k,
  bench_channel_spsc_10k,
  bench_queue_bounded_10k
);
criterion_main!(gate_core);
