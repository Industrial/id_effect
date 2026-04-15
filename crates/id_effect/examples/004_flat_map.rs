//! Ex 004 — `Effect::flat_map` sequences another effect (monadic bind).
use id_effect::{run_blocking, succeed};

fn main() {
  let program = succeed::<i32, (), ()>(20_i32).flat_map(|n| succeed::<i32, (), ()>(n + 22));
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("004_flat_map ok");
}
