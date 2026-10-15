//! Compare a **bind-free** `effect!` body vs a hand-written `sync` step on the 0.7 kernel.
//!
//! A body with no `~` still becomes an async `Effect::from_ctx` program, where `sync` is one
//! lazy synchronous step; the pair shows what the macro costs over the plain constructor. Both
//! run through `Effect::block_on`. Run:
//!
//! ```bash
//! cargo bench -p id_effect --bench effect_macro_bind_free
//! ```

use ::id_effect::{Effect, HNil, effect, sync};
use criterion::{Criterion, criterion_group, criterion_main};

fn bench_bind_free_effect_bang(c: &mut Criterion) {
  c.bench_function("effect_macro_bind_free/effect_bang_no_tilde", |b| {
    b.iter(|| {
      let eff: Effect<usize, (), HNil> = effect!({
        let x = std::hint::black_box(1usize);
        let y = std::hint::black_box(2usize);
        x.wrapping_add(y).wrapping_add(3)
      });
      let out = eff
        .block_on()
        .into_result()
        .expect("bind-free effect should succeed");
      std::hint::black_box(out)
    });
  });
}

fn bench_handwritten_effect_new(c: &mut Criterion) {
  c.bench_function("effect_macro_bind_free/handwritten_effect_new", |b| {
    b.iter(|| {
      let eff: Effect<usize, (), HNil> = sync(move || {
        let x = std::hint::black_box(1usize);
        let y = std::hint::black_box(2usize);
        x.wrapping_add(y).wrapping_add(3)
      });
      let out = eff
        .block_on()
        .into_result()
        .expect("hand-written sync should succeed");
      std::hint::black_box(out)
    });
  });
}

criterion_group!(
  effect_macro_bind_free,
  bench_bind_free_effect_bang,
  bench_handwritten_effect_new
);
criterion_main!(effect_macro_bind_free);
