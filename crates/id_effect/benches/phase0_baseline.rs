//! Phase-0 baseline benches. IDs are unchanged from v0.6 (`docs/benchmarks/v0.6.json`), so
//! `scripts/bench-compare.sh` compares them against the v0.6 baseline.
//!
//! Kernel measured, per bench:
//!
//! - `phase0/bench_effect_map_chain`, `phase0/bench_effect_flat_map_chain`: the 0.7 kernel
//!   (`id_effect::next`), run through its real edge `Effect::block_on` (v0.6 ran
//!   `pollster::block_on(effect.run(&mut ()))`). Same workload: `succeed(1)` then 4 maps / 4
//!   `succeed` binds.
//! - `phase0/bench_env_insert_overhead`: `id_effect::next::DynEnv::insert` (v0.6: `Env::insert`
//!   of `Cap<Bench>`), one insert into a fresh environment.
//! - `phase0/bench_schedule_retry_overhead`: the 0.7 kernel, `Effect::retry` with
//!   `next::Schedule::recurs(3)` through `Effect::block_on` (v0.6: the free `retry` with
//!   `pollster::block_on`). Same workload: two failures, then a success.
//! - `phase0/bench_stream_throughput_collect`: still the v1 kernel (spec k-rebuild §7 Q-C) until
//!   `m-streams` ports it.

use ::id_effect::next::{self as k, DynEnv, Effect, HNil};
use ::id_effect::streaming::stream::Stream;
use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicUsize, Ordering};

fn bench_effect_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_map_chain", |b| {
    b.iter(|| {
      let effect: Effect<usize, (), HNil> = k::succeed(1)
        .map(|n| n + 1)
        .map(|n| n * 2)
        .map(|n| n + 3)
        .map(|n| n * 4);
      let out = effect
        .block_on()
        .into_result()
        .expect("map chain should not fail");
      std::hint::black_box(out)
    });
  });
}

fn bench_effect_flat_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_flat_map_chain", |b| {
    b.iter(|| {
      let effect: Effect<usize, (), HNil> = k::succeed(1)
        .flat_map(|n| k::succeed(n + 1))
        .flat_map(|n| k::succeed(n * 2))
        .flat_map(|n| k::succeed(n + 3))
        .flat_map(|n| k::succeed(n * 4));
      let out = effect
        .block_on()
        .into_result()
        .expect("flat_map chain should not fail");
      std::hint::black_box(out)
    });
  });
}

// v1 body until m-streams
fn bench_stream_throughput_collect(c: &mut Criterion) {
  c.bench_function("phase0/bench_stream_throughput_collect", |b| {
    b.iter(|| {
      let effect = Stream::from_iterable(0..1024)
        .map(|n| n + 1)
        .filter(Box::new(|n: &i32| *n % 2 == 0))
        .run_collect();
      let out = pollster::block_on(effect.run(&mut ())).expect("stream collect should not fail");
      std::hint::black_box(out.len())
    });
  });
}

fn bench_schedule_retry_overhead(c: &mut Criterion) {
  c.bench_function("phase0/bench_schedule_retry_overhead", |b| {
    b.iter(|| {
      let attempts = AtomicUsize::new(0);
      let effect: Effect<usize, &'static str, HNil> = Effect::retry(
        move || {
          let n = attempts.fetch_add(1, Ordering::SeqCst);
          if n < 2 {
            k::fail("boom")
          } else {
            k::succeed(n + 1)
          }
        },
        k::Schedule::recurs(3),
      );
      let out = effect
        .block_on()
        .into_result()
        .expect("retry should eventually succeed");
      std::hint::black_box(out)
    });
  });
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
struct Bench(i32);

fn bench_env_insert_overhead(c: &mut Criterion) {
  c.bench_function("phase0/bench_env_insert_overhead", |b| {
    b.iter(|| {
      let mut env = DynEnv::new();
      env.insert::<Bench>(Bench(7));
      std::hint::black_box(env);
    });
  });
}

criterion_group!(
  phase0_baseline,
  bench_effect_map_chain,
  bench_effect_flat_map_chain,
  bench_stream_throughput_collect,
  bench_schedule_retry_overhead,
  bench_env_insert_overhead
);
criterion_main!(phase0_baseline);
