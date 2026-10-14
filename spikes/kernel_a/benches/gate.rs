//! Spike A gate benches. IDs and workloads mirror the 0.6 baseline benches so that
//! `scripts/bench-compare.sh docs/benchmarks/v0.6.json docs/benchmarks/spike-a.json` applies the
//! D9 gates (`docs/benchmarks/gates.toml`):
//!
//! - `phase0/*` = `crates/id_effect/benches/phase0_baseline.rs` (0.6 runs them with
//!   `pollster::block_on(effect.run(&mut ()))`; here `Effect::block_on`, the real edge).
//! - `gate/async_bind_64`, `gate/flat_map_depth_10k` = `crates/id_effect/benches/gate_core.rs`.
//! - `gate/spawn_join_10k`, `gate/sleep_1k_x_1ms`, `gate/sleep_cpu_1k_x_1ms` =
//!   `crates/id_effect_tokio/benches/gate_fiber.rs` (daemon fibers forked from the edge, then joined).
//! - `kernel_a/need_last_of_16`: report only (a `Selector` read at depth 16 inside an effect).
//!
//! Run: `cargo bench -p kernel_a --bench gate -- --save-baseline spike-a`.

use std::time::Duration;

use cpu_time::ProcessTime;
use criterion::measurement::{Measurement, ValueFormatter};
use criterion::{
  BenchmarkGroup, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use kernel_a::{Effect, Never, Runtime, caps, from_async, hlist, need, sleep, succeed};

const ASYNC_BINDS: usize = 64;
const FLAT_MAP_DEPTH: usize = 10_000;
const SPAWN_FIBERS: u32 = 10_000;
const SLEEP_FIBERS: u32 = 1_000;
const SLEEP: Duration = Duration::from_millis(1);

fn bench_effect_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_map_chain", |b| {
    b.iter(|| {
      let effect: Effect<usize, (), _> = succeed(1)
        .map(|n| n + 1)
        .map(|n| n * 2)
        .map(|n| n + 3)
        .map(|n| n * 4);
      std::hint::black_box(effect.block_on().expect("map chain should not fail"))
    });
  });
}

fn bench_effect_flat_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_flat_map_chain", |b| {
    b.iter(|| {
      let effect: Effect<usize, (), _> = succeed(1)
        .flat_map(|n| succeed(n + 1))
        .flat_map(|n| succeed(n * 2))
        .flat_map(|n| succeed(n + 3))
        .flat_map(|n| succeed(n * 4));
      std::hint::black_box(effect.block_on().expect("flat_map chain should not fail"))
    });
  });
}

fn bench_async_bind_64(c: &mut Criterion) {
  c.bench_function("gate/async_bind_64", |b| {
    b.iter(|| {
      let mut effect: Effect<usize, ()> = succeed(0);
      for _ in 0..ASYNC_BINDS {
        effect = effect.flat_map(|n| from_async(move || async move { Ok(n + 1) }));
      }
      std::hint::black_box(effect.block_on().expect("async bind chain should not fail"))
    });
  });
}

fn bench_flat_map_depth_10k(c: &mut Criterion) {
  c.bench_function("gate/flat_map_depth_10k", |b| {
    b.iter(|| {
      let mut effect: Effect<usize, ()> = succeed(0);
      for _ in 0..FLAT_MAP_DEPTH {
        effect = effect.flat_map(|n| succeed(n + 1));
      }
      std::hint::black_box(effect.block_on().expect("deep flat_map should not fail"))
    });
  });
}

#[derive(Debug)]
struct C(u64);

fn bench_need_depth(c: &mut Criterion) {
  type Env16 = caps!(
    u8, u16, u32, i8, i16, i32, i64, u128, i128, usize, isize, bool, char, f32, f64, C
  );
  let env = hlist![
    1u8,
    2u16,
    3u32,
    4i8,
    5i16,
    6i32,
    7i64,
    8u128,
    9i128,
    10usize,
    11isize,
    true,
    'c',
    1.0f32,
    2.0f64,
    C(16)
  ];
  c.bench_function("kernel_a/need_last_of_16", |b| {
    b.iter(|| {
      let eff: Effect<u64, Never, Env16> = need::<C>().map(|c| c.0);
      std::hint::black_box(eff.provide_all(env.clone()).block_on().expect("need"))
    });
  });
}

/// Criterion measurement of process CPU time (all threads), in nanoseconds (as in gate_fiber.rs).
struct ProcessCpuTime;

impl Measurement for ProcessCpuTime {
  type Intermediate = ProcessTime;
  type Value = Duration;

  fn start(&self) -> Self::Intermediate {
    ProcessTime::now()
  }
  fn end(&self, i: Self::Intermediate) -> Self::Value {
    i.elapsed()
  }
  fn add(&self, v1: &Self::Value, v2: &Self::Value) -> Self::Value {
    *v1 + *v2
  }
  fn zero(&self) -> Self::Value {
    Duration::ZERO
  }
  fn to_f64(&self, value: &Self::Value) -> f64 {
    value.as_nanos() as f64
  }
  fn formatter(&self) -> &dyn ValueFormatter {
    &CpuTimeFormatter
  }
}

struct CpuTimeFormatter;

impl ValueFormatter for CpuTimeFormatter {
  fn scale_values(&self, ns: f64, values: &mut [f64]) -> &'static str {
    let (factor, unit) = if ns < 1e3 {
      (1.0, "ns cpu")
    } else if ns < 1e6 {
      (1e-3, "µs cpu")
    } else if ns < 1e9 {
      (1e-6, "ms cpu")
    } else {
      (1e-9, "s cpu")
    };
    for value in values {
      *value *= factor;
    }
    unit
  }

  fn scale_throughputs(
    &self,
    _typical: f64,
    throughput: &Throughput,
    values: &mut [f64],
  ) -> &'static str {
    let per_iter = match *throughput {
      Throughput::Bits(n)
      | Throughput::Bytes(n)
      | Throughput::BytesDecimal(n)
      | Throughput::Elements(n) => n,
      Throughput::ElementsAndBytes { elements, .. } => elements,
    } as f64;
    for value in values {
      *value = per_iter * 1e9 / value.max(1.0);
    }
    "elem/cpu-s"
  }

  fn scale_for_machines(&self, _values: &mut [f64]) -> &'static str {
    "ns"
  }
}

fn configure<M: Measurement>(group: &mut BenchmarkGroup<'_, M>) {
  group.sampling_mode(SamplingMode::Flat);
  group.sample_size(10);
}

fn sleep_fibers(rt: &Runtime) {
  rt.block_on_future(async {
    let handles: Vec<_> = (0..SLEEP_FIBERS)
      .map(|_| rt.fork(sleep::<(), _>(SLEEP)))
      .collect();
    for h in handles {
      assert!(h.join_future().await.is_ok(), "sleep fiber failed");
    }
  })
  .expect("bench edge is not inside a task");
}

fn bench_wall(c: &mut Criterion) {
  let rt = Runtime::new().expect("runtime");
  let mut group = c.benchmark_group("gate");
  configure(&mut group);
  group.bench_function("spawn_join_10k", |b| {
    b.iter(|| {
      rt.block_on_future(async {
        let handles: Vec<_> = (0..SPAWN_FIBERS)
          .map(|i| rt.fork(succeed::<u32, (), _>(i)))
          .collect();
        let mut sum = 0u64;
        for h in handles {
          sum += u64::from(h.join_future().await.expect("spawn fiber failed"));
        }
        std::hint::black_box(sum)
      })
      .expect("bench edge")
    });
  });
  group.bench_function("sleep_1k_x_1ms", |b| b.iter(|| sleep_fibers(&rt)));
  group.finish();
}

fn bench_cpu(c: &mut Criterion<ProcessCpuTime>) {
  let rt = Runtime::new().expect("runtime");
  let mut group = c.benchmark_group("gate");
  configure(&mut group);
  group.bench_function("sleep_cpu_1k_x_1ms", |b| b.iter(|| sleep_fibers(&rt)));
  group.finish();
}

criterion_group!(
  core,
  bench_effect_map_chain,
  bench_effect_flat_map_chain,
  bench_async_bind_64,
  bench_flat_map_depth_10k,
  bench_need_depth
);
criterion_group!(wall, bench_wall);
criterion_group! {
  name = cpu;
  config = Criterion::default().with_measurement(ProcessCpuTime);
  targets = bench_cpu
}
criterion_main!(core, wall, cpu);
