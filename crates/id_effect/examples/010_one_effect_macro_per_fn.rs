//! Ex 010 — One top-level `effect!` per function; control flow stays inside the block.
use id_effect::{effect, run_blocking, succeed};

fn branch(flag: bool) -> id_effect::Effect<i32, (), ()> {
  effect!(|_r: &mut ()| {
    if flag {
      let x = ~succeed(40_i32);
      x + 2
    } else {
      let x = ~succeed(41_i32);
      x + 1
    }
  })
}

fn main() {
  assert_eq!(run_blocking(branch(true), ()), Ok(42));
  assert_eq!(run_blocking(branch(false), ()), Ok(42));
  println!("010_one_effect_macro_per_fn ok");
}
