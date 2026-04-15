//! Compare **bind-free** [`effect!`](::id_effect::effect) expansion vs hand-written [`Effect::new`](::id_effect::Effect::new).
//!
//! After the proc-macro fast path (no `~` → `Effect::new`), both should compile to very similar
//! code and show comparable timings. Run:
//!
//! ```bash
//! cargo bench -p id_effect --bench effect_macro_bind_free
//! ```

use ::id_effect::{Effect, effect};
use criterion::{Criterion, criterion_group, criterion_main};

fn bench_bind_free_effect_bang(c: &mut Criterion) {
  c.bench_function("effect_macro_bind_free/effect_bang_no_tilde", |b| {
    b.iter(|| {
      let eff: Effect<usize, (), ()> = effect!(|_r: &mut ()| {
        let x = std::hint::black_box(1usize);
        let y = std::hint::black_box(2usize);
        x.wrapping_add(y).wrapping_add(3)
      });
      let out = pollster::block_on(eff.run(&mut ())).expect("bind-free effect should succeed");
      std::hint::black_box(out)
    });
  });
}

fn bench_handwritten_effect_new(c: &mut Criterion) {
  c.bench_function("effect_macro_bind_free/handwritten_effect_new", |b| {
    b.iter(|| {
      let eff: Effect<usize, (), ()> = Effect::new(move |_r: &mut ()| {
        let x = std::hint::black_box(1usize);
        let y = std::hint::black_box(2usize);
        Ok(x.wrapping_add(y).wrapping_add(3))
      });
      let out = pollster::block_on(eff.run(&mut ())).expect("hand-written new should succeed");
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
