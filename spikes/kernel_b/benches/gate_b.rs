//! Spike B gate benches. IDs match `docs/benchmarks/gates.toml`; the workloads mirror
//! `crates/id_effect/benches/{phase0_baseline,gate_core}.rs` and
//! `crates/id_effect_tokio/benches/gate_fiber.rs` (same sizes, same build-then-run shape).
//!
//! Differences forced by the design, not by tuning:
//! - 0.6 runs the bind benches with `pollster::block_on(effect.run(&mut ()))`. Spike B runs them
//!   with `Effect::block_on`, which returns a `Ready` effect without an executor and otherwise
//!   spawns a root fiber on a fresh `ParkExecutor` (so `async_bind_64` pays fiber setup per
//!   iteration).
//! - `map`/`flat_map` on a `Ready` effect apply eagerly (R1 inline Ready), so the map/flat_map
//!   chains and `flat_map_depth_10k` measure the eager path.
//!
//! Extra (not in gates.toml): `b/interrupt_sleeping_fiber` (fork a fiber sleeping 10 s, then
//! interrupt it and await its exit).

use std::time::Duration;

use cpu_time::ProcessTime;
use criterion::measurement::{Measurement, ValueFormatter};
use criterion::{
  BenchmarkGroup, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use kernel_b::{Effect, Runtime, effect, from_async, sleep, succeed};

const ASYNC_BINDS: usize = 64;
const FLAT_MAP_DEPTH: usize = 10_000;
const SPAWN_FIBERS: u32 = 10_000;
const SLEEP_FIBERS: u32 = 1_000;
const SLEEP: Duration = Duration::from_millis(1);

fn bench_effect_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_map_chain", |b| {
    b.iter(|| {
      let effect = succeed::<usize, (), ()>(1)
        .map(|n| n + 1)
        .map(|n| n * 2)
        .map(|n| n + 3)
        .map(|n| n * 4);
      let out = effect.block_on().expect("map chain should not fail");
      std::hint::black_box(out)
    });
  });
}

fn bench_effect_flat_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_flat_map_chain", |b| {
    b.iter(|| {
      let effect = succeed::<usize, (), ()>(1)
        .flat_map(|n| succeed::<usize, (), ()>(n + 1))
        .flat_map(|n| succeed::<usize, (), ()>(n * 2))
        .flat_map(|n| succeed::<usize, (), ()>(n + 3))
        .flat_map(|n| succeed::<usize, (), ()>(n * 4));
      let out = effect.block_on().expect("flat_map chain should not fail");
      std::hint::black_box(out)
    });
  });
}

fn bench_async_bind_64(c: &mut Criterion) {
  c.bench_function("gate/async_bind_64", |b| {
    b.iter(|| {
      let mut effect: Effect<usize, (), ()> = succeed(0);
      for _ in 0..ASYNC_BINDS {
        effect = effect.flat_map(|n| from_async(async move { Ok(n + 1) }));
      }
      let out = effect.block_on().expect("async bind chain should not fail");
      std::hint::black_box(out)
    });
  });
}

fn bench_flat_map_depth_10k(c: &mut Criterion) {
  c.bench_function("gate/flat_map_depth_10k", |b| {
    b.iter(|| {
      let mut effect: Effect<usize, (), ()> = succeed(0);
      for _ in 0..FLAT_MAP_DEPTH {
        effect = effect.flat_map(|n| succeed(n + 1));
      }
      let out = effect.block_on().expect("deep flat_map should not fail");
      std::hint::black_box(out)
    });
  });
}

fn bench_interrupt_sleeping_fiber(c: &mut Criterion) {
  c.bench_function("b/interrupt_sleeping_fiber", |b| {
    b.iter(|| {
      let eff: Effect<bool, (), ()> = effect!(|r| {
        let h = ~ sleep::<(), ()>(Duration::from_secs(10)).fork();
        let exit = ~ h.interrupt();
        exit.is_err()
      });
      std::hint::black_box(eff.block_on().expect("interrupt bench"))
    });
  });
}

// ── fibers on tokio (gate_fiber.rs) ────────────────────────────────────────────────────────

/// Criterion measurement of process CPU time (all threads), in nanoseconds.
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

fn tokio_rt() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime")
}

fn sleep_fibers(tokio: &tokio::runtime::Runtime, rt: &Runtime) {
  tokio.block_on(async {
    let handles: Vec<_> = (0..SLEEP_FIBERS)
      .map(|_| rt.spawn(sleep::<(), ()>(SLEEP), ()))
      .collect();
    for handle in handles {
      assert!(handle.exit().await.is_ok(), "sleep fiber failed");
    }
  });
}

fn bench_wall(c: &mut Criterion) {
  let tokio = tokio_rt();
  let rt = Runtime::tokio(tokio.handle().clone());
  let mut group = c.benchmark_group("gate");
  configure(&mut group);

  group.bench_function("spawn_join_10k", |b| {
    b.iter(|| {
      tokio.block_on(async {
        let handles: Vec<_> = (0..SPAWN_FIBERS)
          .map(|i| rt.spawn(succeed::<u32, (), ()>(i), ()))
          .collect();
        let mut sum = 0u64;
        for handle in handles {
          sum += u64::from(handle.exit().await.expect("spawn fiber failed"));
        }
        std::hint::black_box(sum)
      })
    });
  });

  group.bench_function("sleep_1k_x_1ms", |b| b.iter(|| sleep_fibers(&tokio, &rt)));
  group.finish();
}

fn bench_cpu(c: &mut Criterion<ProcessCpuTime>) {
  let tokio = tokio_rt();
  let rt = Runtime::tokio(tokio.handle().clone());
  let mut group = c.benchmark_group("gate");
  configure(&mut group);
  group.bench_function("sleep_cpu_1k_x_1ms", |b| {
    b.iter(|| sleep_fibers(&tokio, &rt))
  });
  group.finish();
}

criterion_group!(
  binds,
  bench_effect_map_chain,
  bench_effect_flat_map_chain,
  bench_async_bind_64,
  bench_flat_map_depth_10k,
  bench_interrupt_sleeping_fiber
);
criterion_group!(wall, bench_wall);
criterion_group! {
  name = cpu;
  config = Criterion::default().with_measurement(ProcessCpuTime);
  targets = bench_cpu
}
criterion_main!(binds, wall, cpu);
