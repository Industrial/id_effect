//! Ex 007 — Discard a unit effect with `~expr;`.
use id_effect::{Effect, effect, run_blocking, succeed};

fn main() {
  let program: Effect<i32, (), ()> = effect! {
    ~succeed(());
    ~succeed(());
    42_i32
  };
  assert_eq!(run_blocking(program, ()), Ok(42));
  println!("007_effect_macro_discard ok");
}
