//! Ex 011 — `map_error` rewrites the failure type.
use id_effect::{Effect, effect, run_blocking, succeed};

fn parse_i32(raw: &'static str) -> Effect<i32, &'static str, ()> {
  effect! {
    let v = ~raw.parse::<i32>().map_err(|_| "parse_failed");
    v
  }
}

fn main() {
  let program = effect! {
    let raw = ~succeed::<&'static str, &'static str, ()>("nope");
    let v = ~parse_i32(raw).map_error(|_| "bad_input");
    v
  };
  assert_eq!(run_blocking(program, ()), Err("bad_input"));
  println!("011_map_error ok");
}
