//! Ex 009 — `Result` implements `IntoBind` inside `effect!`.
use id_effect::{Effect, effect, run_blocking};

fn main() {
  let program: Effect<i32, (), ()> = effect! {
    let n = ~Ok::<i32, ()>(42);
    n
  };
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("009_result_into_bind ok");
}
