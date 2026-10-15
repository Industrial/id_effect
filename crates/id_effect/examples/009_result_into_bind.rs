//! Ex 009 — Inside `effect_next!`, a `Result` short-circuits with `?` into the typed error.
use id_effect::next::{Effect, effect_next};
use id_effect::{Cause, Exit};

fn parse(raw: &'static str) -> Effect<i32, &'static str> {
  effect_next!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
}

fn main() {
  assert_eq!(parse("42").block_on(), Exit::succeed(42));
  assert_eq!(
    parse("x").block_on(),
    Exit::Failure(Cause::Fail("parse_failed"))
  );
  println!("009_result_into_bind ok");
}
