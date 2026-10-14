//! 0.7 gate benches for the core crate (release plan W0, `b0-bench-core`).
//!
//! Every benchmark ID here is compared against the `bench/v0.6` baseline by
//! `scripts/bench-compare.sh` using the thresholds in `docs/benchmarks/gates.toml`. Keep the IDs
//! stable; change the bodies only when an API migration forces it. Run:
//!
//! ```bash
//! cargo bench -p id_effect --bench gate_core
//! ```

use ::id_effect::{Channel, Effect, Queue, Stream, from_async, pure, succeed};
use criterion::{Criterion, criterion_group, criterion_main};

const ASYNC_BINDS: usize = 64;
const FLAT_MAP_DEPTH: usize = 10_000;
const STREAM_PULL_LEN: u64 = 10_000;
const STREAM_MAP_EFFECT_LEN: u64 = 1_000;
const COORD_MESSAGES: u64 = 10_000;
const QUEUE_CAPACITY: usize = 64;

fn current_thread_runtime() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_current_thread()
    .build()
    .expect("current_thread runtime should build")
}

/// 64 sequential binds, each through a real `async` step (`from_async`).
fn bench_async_bind_64(c: &mut Criterion) {
  c.bench_function("gate/async_bind_64", |b| {
    b.iter(|| {
      let mut effect: Effect<usize, (), ()> = pure(0);
      for _ in 0..ASYNC_BINDS {
        effect = effect.flat_map(|n| from_async(move |_r: &mut ()| async move { Ok(n + 1) }));
      }
      let out = pollster::block_on(effect.run(&mut ())).expect("async bind chain should not fail");
      std::hint::black_box(out)
    });
  });
}

/// A left-nested `flat_map` chain 10k deep (build plus run).
fn bench_flat_map_depth_10k(c: &mut Criterion) {
  c.bench_function("gate/flat_map_depth_10k", |b| {
    b.iter(|| {
      let mut effect: Effect<usize, (), ()> = pure(0);
      for _ in 0..FLAT_MAP_DEPTH {
        effect = effect.flat_map(|n| succeed(n + 1));
      }
      let out = pollster::block_on(effect.run(&mut ())).expect("deep flat_map should not fail");
      std::hint::black_box(out)
    });
  });
}

/// Pull 10k elements through a pure `map` stage into a collected vector.
fn bench_stream_pull_map_10k(c: &mut Criterion) {
  c.bench_function("stream/pull_map_10k", |b| {
    b.iter(|| {
      let effect = Stream::from_iterable(0..STREAM_PULL_LEN)
        .map(|n| n.wrapping_mul(3))
        .run_collect();
      let out: Vec<u64> =
        pollster::block_on(effect.run(&mut ())).expect("stream pull should not fail");
      std::hint::black_box(out.len())
    });
  });
}

/// Run an effect per element for 1k elements via `map_effect`.
fn bench_stream_map_effect_1k(c: &mut Criterion) {
  c.bench_function("stream/map_effect_1k", |b| {
    b.iter(|| {
      let effect = Stream::from_iterable(0..STREAM_MAP_EFFECT_LEN)
        .map_effect(|n| succeed::<u64, (), ()>(n.wrapping_mul(3)))
        .run_collect();
      let out: Vec<u64> =
        pollster::block_on(effect.run(&mut ())).expect("stream map_effect should not fail");
      std::hint::black_box(out.len())
    });
  });
}

/// One producer writes 10k elements into a queue-backed `Channel`; one consumer reads them back.
fn bench_channel_spsc_10k(c: &mut Criterion) {
  let rt = current_thread_runtime();
  c.bench_function("coord/channel_spsc_10k", |b| {
    b.iter(|| {
      rt.block_on(async {
        let queue = Queue::<u64>::unbounded()
          .run(&mut ())
          .await
          .expect("unbounded queue should build");
        let channel: Channel<u64, u64, (), (), ()> = Channel::from_queue_and_map(queue, |n| n);
        let producer = {
          let channel = channel.clone();
          async move {
            for n in 0..COORD_MESSAGES {
              channel
                .write(n)
                .run(&mut ())
                .await
                .expect("channel write should not fail");
            }
          }
        };
        let consumer = async {
          let mut sum = 0u64;
          for _ in 0..COORD_MESSAGES {
            let n = channel
              .read()
              .run(&mut ())
              .await
              .expect("channel read should not fail")
              .expect("channel should not end early");
            sum = sum.wrapping_add(n);
          }
          sum
        };
        let ((), sum) = tokio::join!(producer, consumer);
        std::hint::black_box(sum)
      })
    });
  });
}

/// One producer offers 10k elements into a bounded queue (capacity 64, yielding when full);
/// one consumer takes them.
fn bench_queue_bounded_10k(c: &mut Criterion) {
  let rt = current_thread_runtime();
  c.bench_function("coord/queue_bounded_10k", |b| {
    b.iter(|| {
      rt.block_on(async {
        let queue = Queue::<u64>::bounded(QUEUE_CAPACITY)
          .run(&mut ())
          .await
          .expect("bounded queue should build");
        let producer = {
          let queue = queue.clone();
          async move {
            for n in 0..COORD_MESSAGES {
              while !queue
                .offer(n)
                .run(&mut ())
                .await
                .expect("queue offer is infallible")
              {
                tokio::task::yield_now().await;
              }
            }
          }
        };
        let consumer = async {
          let mut sum = 0u64;
          for _ in 0..COORD_MESSAGES {
            let n = queue
              .take()
              .run(&mut ())
              .await
              .expect("queue take should not fail");
            sum = sum.wrapping_add(n);
          }
          sum
        };
        let ((), sum) = tokio::join!(producer, consumer);
        std::hint::black_box(sum)
      })
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
