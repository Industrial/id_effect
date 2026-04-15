//! Ex 003 — `Effect::map` transforms success without changing the environment.
use id_effect::{run_blocking, succeed};

fn main() {
  let program = succeed::<i32, (), ()>(21_i32).map(|n| n * 2);
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("003_map ok");
}
