use ::id_effect::scheduling::schedule::{Schedule, retry};
use ::id_effect::streaming::stream::Stream;
use ::id_effect::{Cap, Env, fail, pure, succeed};
use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::atomic::{AtomicUsize, Ordering};

fn bench_effect_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_map_chain", |b| {
    b.iter(|| {
      let effect = pure::<usize>(1)
        .map(|n| n + 1)
        .map(|n| n * 2)
        .map(|n| n + 3)
        .map(|n| n * 4);
      let out = pollster::block_on(effect.run(&mut ())).expect("map chain should not fail");
      std::hint::black_box(out)
    });
  });
}

fn bench_effect_flat_map_chain(c: &mut Criterion) {
  c.bench_function("phase0/bench_effect_flat_map_chain", |b| {
    b.iter(|| {
      let effect = pure::<usize>(1)
        .flat_map(|n| succeed::<usize, (), ()>(n + 1))
        .flat_map(|n| succeed::<usize, (), ()>(n * 2))
        .flat_map(|n| succeed::<usize, (), ()>(n + 3))
        .flat_map(|n| succeed::<usize, (), ()>(n * 4));
      let out = pollster::block_on(effect.run(&mut ())).expect("flat_map chain should not fail");
      std::hint::black_box(out)
    });
  });
}

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
      let effect = retry(
        move || {
          let n = attempts.fetch_add(1, Ordering::SeqCst);
          if n < 2 {
            fail::<usize, &'static str, ()>("boom")
          } else {
            succeed::<usize, &'static str, ()>(n + 1)
          }
        },
        Schedule::recurs(3),
      );
      let out = pollster::block_on(effect.run(&mut ())).expect("retry should eventually succeed");
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
      let mut env = Env::new();
      env.insert::<Cap<Bench>>(Bench(7));
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
