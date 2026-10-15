//! Ex 010 — One `effect_next!` per function; control flow stays inside the block.
use id_effect::Exit;
use id_effect::next::{Effect, effect_next, succeed};

fn number(n: i32) -> Effect<i32> {
  succeed(n)
}

fn branch(flag: bool) -> Effect<i32> {
  effect_next!({
    if flag {
      let x = ~number(40);
      x + 2
    } else {
      let x = ~number(41);
      x + 1
    }
  })
}

fn main() {
  assert_eq!(branch(true).block_on(), Exit::succeed(42));
  assert_eq!(branch(false).block_on(), Exit::succeed(42));
  println!("010_one_effect_macro_per_fn ok");
}
