//! Ex 001 — `Effect` as a lazy value (`succeed` / `run_blocking` at the edge).
use id_effect::{Effect, effect, run_blocking, succeed};

fn main() {
  let program: Effect<i32, (), ()> = effect! {
    let a = ~succeed(40_i32);
    let b = ~succeed(2_i32);
    a + b
  };
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("001_effect_value ok");
}
