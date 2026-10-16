// Gallery: a parallel join `~(a, b)` in `effect_local!` (branches run on child fibers).
// Fix: write the body with `effect!`, or bind the effects one at a time.
use id_effect::Never;
use id_effect::{Effect, LocalEffect, effect_local, succeed};

fn one() -> Effect<u8, Never> {
  succeed(1)
}

fn local() -> LocalEffect<u8, Never> {
  effect_local!({
    let (a, b) = ~(one(), one());
    a + b
  })
}

fn main() {
  let _ = local();
}
