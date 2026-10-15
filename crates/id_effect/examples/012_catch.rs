//! Ex 012 — `catch` recovers from a typed failure.
use id_effect::Exit;
use id_effect::next::{Effect, effect_next, succeed};

fn parse_i32(raw: &'static str) -> Effect<i32, &'static str> {
  effect_next!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
}

fn main() {
  let program: Effect<i32, &'static str> = effect_next!({
    let v = ~parse_i32("x").catch(|_| succeed(0));
    v + 42
  });
  assert_eq!(program.block_on(), Exit::succeed(42));
  println!("012_catch ok");
}
