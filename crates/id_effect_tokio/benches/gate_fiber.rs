//! Fiber gate benches (D9) on `TokioRuntime`: fiber spawn/join and fiber sleep, wall clock and
//! process CPU time.
//!
//! - `gate/spawn_join_10k` — wall clock to fork 10 000 trivial fibers with `run_fork` and join
//!   them all.
//! - `gate/sleep_1k_x_1ms` — wall clock for 1 000 concurrent fibers that each sleep 1 ms through
//!   `Runtime::sleep`.
//! - `gate/sleep_cpu_1k_x_1ms` — the same sleep workload, measured in **process CPU time** (user +
//!   system, all threads) by the custom criterion `Measurement` [`ProcessCpuTime`]. Values are CPU
//!   nanoseconds per iteration.
//!
//! What the CPU bench proves: in 0.6 a forked fiber is driven by `run_blocking` on Tokio's
//! blocking pool, which polls with a noop waker and calls `thread::yield_now` on every `Pending`.
//! A sleeping fiber therefore busy-spins an OS thread for its whole sleep. Wall clock hides this
//! (the sleep still ends after about 1 ms); CPU time does not, because every spinning thread
//! burns CPU for the full sleep duration. A kernel with real wakers parks sleeping fibers, so its
//! CPU time drops to the spawn and timer bookkeeping cost. The D9 gate requires a `sleep_cpu`
//! ratio below 1.00 against the v0.6 baseline.

use std::sync::Arc;
use std::time::Duration;

use cpu_time::ProcessTime;
use criterion::measurement::{Measurement, ValueFormatter};
use criterion::{
  BenchmarkGroup, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use id_effect::kernel::succeed;
use id_effect::{Runtime, run_fork};
use id_effect_tokio::TokioRuntime;

const SPAWN_FIBERS: u32 = 10_000;
const SLEEP_FIBERS: u32 = 1_000;
const SLEEP: Duration = Duration::from_millis(1);

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

/// Formats CPU nanoseconds like criterion's wall-clock formatter, with a `cpu` unit suffix.
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

/// Each iteration costs seconds on 0.6 (10k spawns), so sample flat with criterion's minimum
/// sample count; linear sampling would need at least 55 iterations for 10 samples instead of 10.
fn configure<M: Measurement>(group: &mut BenchmarkGroup<'_, M>) {
  group.sampling_mode(SamplingMode::Flat);
  group.sample_size(10);
}

fn runtime() -> Arc<TokioRuntime> {
  Arc::new(TokioRuntime::new_multi_thread().expect("tokio runtime"))
}

/// Fork `SLEEP_FIBERS` fibers that each sleep `SLEEP` on `rt`, then join them all.
fn sleep_fibers(rt: &Arc<TokioRuntime>) {
  rt.block_on(async {
    let handles: Vec<_> = (0..SLEEP_FIBERS)
      .map(|_| {
        let fiber_rt = Arc::clone(rt);
        run_fork(&**rt, move || (fiber_rt.sleep(SLEEP), ()))
      })
      .collect();
    for handle in handles {
      assert!(handle.join().await.is_ok(), "sleep fiber failed");
    }
  });
}

fn bench_wall(c: &mut Criterion) {
  let rt = runtime();
  let mut group = c.benchmark_group("gate");
  configure(&mut group);

  group.bench_function("spawn_join_10k", |b| {
    b.iter(|| {
      rt.block_on(async {
        let handles: Vec<_> = (0..SPAWN_FIBERS)
          .map(|i| run_fork(&*rt, move || (succeed::<u32, (), ()>(i), ())))
          .collect();
        let mut sum = 0u64;
        for handle in handles {
          sum += u64::from(handle.join().await.expect("spawn fiber failed"));
        }
        std::hint::black_box(sum)
      })
    });
  });

  group.bench_function("sleep_1k_x_1ms", |b| b.iter(|| sleep_fibers(&rt)));
  group.finish();
}

fn bench_cpu(c: &mut Criterion<ProcessCpuTime>) {
  let rt = runtime();
  let mut group = c.benchmark_group("gate");
  configure(&mut group);
  group.bench_function("sleep_cpu_1k_x_1ms", |b| b.iter(|| sleep_fibers(&rt)));
  group.finish();
}

criterion_group!(wall, bench_wall);
criterion_group! {
  name = cpu;
  config = Criterion::default().with_measurement(ProcessCpuTime);
  targets = bench_cpu
}
criterion_main!(wall, cpu);
