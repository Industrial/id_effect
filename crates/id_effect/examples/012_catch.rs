//! Ex 012 — `catch` recovers from a typed failure.
use id_effect::{Effect, effect, run_blocking, succeed};

fn parse_i32(raw: &'static str) -> Effect<i32, &'static str, ()> {
  effect! {
    let v = ~raw.parse::<i32>().map_err(|_| "parse_failed");
    v
  }
}

fn main() {
  let program: Effect<i32, &'static str, ()> = effect! {
    let raw = ~succeed::<&'static str, &'static str, ()>("x");
    let v = ~parse_i32(raw).catch(|_| succeed::<i32, &'static str, ()>(0_i32));
    v + 42
  };
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("012_catch ok");
}
