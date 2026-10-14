//! Startup cost of `run_with` on a pure effect, including default Compute Fabric setup.
//!
//! `cold` uninstalls the fabric before every iteration so each run builds and installs the
//! default fabric on a fresh thread (the per-process startup path); `warm` reuses it.
//! Target: `cold` well under 50 ms (telemetry must not be sampled at fabric construction).

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::compute::uninstall_fabric;
use id_effect::kernel::succeed;
use id_effect::{ProviderBox, run_with};

fn run_pure() -> u32 {
  run_with(Vec::<ProviderBox>::new(), succeed::<u32, (), ()>(7)).expect("run_with")
}

fn bench_startup_run_with(c: &mut Criterion) {
  c.bench_function("startup_run_with_cold", |b| {
    b.iter(|| {
      uninstall_fabric();
      let out = std::thread::spawn(run_pure).join().expect("bench thread");
      std::hint::black_box(out)
    });
  });

  c.bench_function("startup_run_with_warm", |b| {
    let _ = run_pure();
    b.iter(|| std::hint::black_box(run_pure()));
  });
}

criterion_group!(benches, bench_startup_run_with);
criterion_main!(benches);
