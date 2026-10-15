//! Ex 011 — `map_err` rewrites the typed failure.
use id_effect::next::{Effect, effect_next};
use id_effect::{Cause, Exit};

fn parse_i32(raw: &'static str) -> Effect<i32, &'static str> {
  effect_next!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
}

fn main() {
  let program: Effect<i32, &'static str> = effect_next!({
    let v = ~parse_i32("nope").map_err(|_| "bad_input");
    v
  });
  assert_eq!(program.block_on(), Exit::Failure(Cause::Fail("bad_input")));
  println!("011_map_error ok");
}
