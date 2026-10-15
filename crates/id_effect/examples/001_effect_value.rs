//! Ex 001 — `Effect` as a lazy value (`succeed`, run at the edge with `block_on`).
use id_effect::{Effect, effect, succeed};
use id_effect::{Exit, Never};

fn number(n: i32) -> Effect<i32> {
  succeed(n)
}

fn program() -> Effect<i32, Never> {
  effect!({
    let a = ~number(40);
    let b = ~number(2);
    a + b
  })
}

fn main() {
  assert_eq!(program().block_on(), Exit::succeed(42));
  println!("001_effect_value ok");
}
