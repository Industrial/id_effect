//! Ex 009 — Inside `effect!`, a `Result` short-circuits with `?` into the typed error.
use id_effect::{Cause, Exit};
use id_effect::{Effect, effect};

fn parse(raw: &'static str) -> Effect<i32, &'static str> {
  effect!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
}

fn main() {
  assert_eq!(parse("42").block_on(), Exit::succeed(42));
  assert_eq!(
    parse("x").block_on(),
    Exit::Failure(Cause::Fail("parse_failed"))
  );
  println!("009_result_into_bind ok");
}
