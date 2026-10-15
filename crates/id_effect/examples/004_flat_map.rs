//! Ex 004 — `Effect::flat_map` sequences another effect (monadic bind).
use id_effect::Exit;
use id_effect::next::{Effect, succeed};

fn main() {
  let program: Effect<i32> = succeed(20).flat_map(|n: i32| succeed(n + 22));
  assert_eq!(program.block_on(), Exit::succeed(42));
  println!("004_flat_map ok");
}
