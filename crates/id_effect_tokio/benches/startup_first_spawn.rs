//! Startup cost of the first `TokioRuntime` fiber spawn of a trivial effect.
//!
//! Each iteration builds a fresh current-thread `TokioRuntime` (a tokio runtime plus the
//! `TokioExecutor`) and forks and joins one fiber, so it pays runtime construction plus the first
//! spawn. Target: well under 50 ms.
//!
//! In 0.6 this also uninstalled the Compute Fabric so the spawn paid default-fabric setup and the
//! first supervisor tick; since 0.7 (`m-tokio`) `TokioRuntime` no longer owns a fabric (admission
//! is `compute`'s job), so the bench ID is unchanged but the fabric step is gone.

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::{HNil, succeed};
use id_effect_tokio::TokioRuntime;

fn bench_startup_first_spawn(c: &mut Criterion) {
  c.bench_function("startup_tokio_first_spawn", |b| {
    b.iter(|| {
      let rt = TokioRuntime::new_current_thread().expect("tokio runtime");
      let out = rt.block_on(rt.fork(succeed::<u32, (), HNil>(7)).join_future());
      std::hint::black_box(out)
    });
  });
}

criterion_group!(benches, bench_startup_first_spawn);
criterion_main!(benches);
