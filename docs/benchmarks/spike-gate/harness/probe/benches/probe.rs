//! Scaling probe: the gate/async_bind_64 body at n = 1, 16, 64, 256 for v0.6, A and B, plus a
//! black_box'd 10k sync flat_map chain (B's gate version constant-folds). Default criterion config.
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use spike_gate_probe::{a, b, v06};

fn async_scaling(c: &mut Criterion) {
  let mut g = c.benchmark_group("probe/async_bind");
  for n in [1usize, 16, 64, 256] {
    g.bench_with_input(BenchmarkId::new("v06", n), &n, |bch, &n| {
      bch.iter(|| v06::async_chain(n))
    });
    g.bench_with_input(BenchmarkId::new("a", n), &n, |bch, &n| {
      bch.iter(|| a::async_chain(n))
    });
    g.bench_with_input(BenchmarkId::new("b", n), &n, |bch, &n| {
      bch.iter(|| b::async_chain(n))
    });
  }
  g.finish();
}

fn sync_bb(c: &mut Criterion) {
  let mut g = c.benchmark_group("probe/flat_map_depth_10k_bb");
  g.bench_function("v06", |bch| bch.iter(|| v06::sync_chain(10_000)));
  g.bench_function("a", |bch| bch.iter(|| a::sync_chain(10_000)));
  g.bench_function("b", |bch| bch.iter(|| b::sync_chain(10_000)));
  g.finish();
}

criterion_group!(probe, async_scaling, sync_bb);
criterion_main!(probe);
