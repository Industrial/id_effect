//! Startup cost of the first `TokioRuntime` fiber spawn of a trivial effect.
//!
//! Each iteration uninstalls the Compute Fabric and builds a fresh `TokioRuntime`, so the
//! spawn pays default-fabric setup plus the first supervisor tick (first telemetry sample).
//! Target: well under 50 ms (the first sample must not sleep for a CPU-usage interval).

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::compute::uninstall_fabric;
use id_effect::kernel::succeed;
use id_effect::run_fork;
use id_effect_tokio::TokioRuntime;

fn bench_startup_first_spawn(c: &mut Criterion) {
  c.bench_function("startup_tokio_first_spawn", |b| {
    b.iter(|| {
      uninstall_fabric();
      let rt = TokioRuntime::new_current_thread().expect("tokio runtime");
      let out = rt.block_on(async {
        run_fork(&rt, || (succeed::<u32, (), ()>(7), ()))
          .join()
          .await
      });
      std::hint::black_box(out)
    });
  });
}

criterion_group!(benches, bench_startup_first_spawn);
criterion_main!(benches);
