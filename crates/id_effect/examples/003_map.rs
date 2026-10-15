//! Ex 003 — `Effect::map` transforms the success value.
use id_effect::Exit;
use id_effect::next::{Effect, succeed};

fn main() {
  let program: Effect<i32> = succeed(21).map(|n: i32| n * 2);
  assert_eq!(program.block_on(), Exit::succeed(42));
  println!("003_map ok");
}
