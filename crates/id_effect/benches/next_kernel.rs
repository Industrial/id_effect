//! Report-only benches for the 0.7 kernel (spec k-rebuild §3.10 AC4). Not in
//! `docs/benchmarks/gates.toml`; `k-runloop-opt` reads them as probes.
//!
//! - `kernel_next/need_last_of_16`: a typed-`R` `need` read at depth 16 inside an effect, provided
//!   and run at the edge (spike A `spikes/kernel_a/benches/gate.rs`, `kernel_a/need_last_of_16`).
//! - `next_probe/async_bind_n{1,16,64,256}`: the `gate/async_bind_64` body at n binds through
//!   `from_async` (the spike-gate probe shape, `docs/benchmarks/spike-gate/harness/probe`), so the
//!   per-bind marginal cost and the fixed edge cost can be separated.
//!
//! Run: `cargo bench -p id_effect --bench next_kernel`.

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::{Effect, HNil, from_async, hlist, need, succeed};

const PROBE_BINDS: [usize; 4] = [1, 16, 64, 256];

#[derive(Debug)]
struct C(u64);

fn bench_need_depth(c: &mut Criterion) {
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
  c.bench_function("kernel_next/need_last_of_16", |b| {
    b.iter(|| {
      let eff: Effect<u64, (), HNil> = need::<C>().map(|c| c.0).provide_all(env.clone());
      std::hint::black_box(eff.block_on().into_result().expect("need"))
    });
  });
}

fn async_chain(n: usize) -> usize {
  let mut effect: Effect<usize, (), HNil> = succeed(0);
  for _ in 0..n {
    effect = effect.flat_map(|n| from_async(move || async move { Ok(n + 1) }));
  }
  effect
    .block_on()
    .into_result()
    .expect("async bind chain should not fail")
}

fn bench_async_bind_probe(c: &mut Criterion) {
  let mut group = c.benchmark_group("next_probe");
  for n in PROBE_BINDS {
    group.bench_function(format!("async_bind_n{n}"), |b| {
      b.iter(|| std::hint::black_box(async_chain(std::hint::black_box(n))))
    });
  }
  group.finish();
}

criterion_group!(next_kernel, bench_need_depth, bench_async_bind_probe);
criterion_main!(next_kernel);
