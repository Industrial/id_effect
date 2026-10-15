//! Ex 006 — `effect_next!` binds a sub-effect with `let x = ~expr`.
use id_effect::Exit;
use id_effect::next::{Effect, effect_next, succeed};

fn number(n: i32) -> Effect<i32> {
  succeed(n)
}

fn main() {
  let program: Effect<i32> = effect_next!({
    let x = ~number(6);
    let y = ~number(7);
    x * y
  });
  assert_eq!(program.block_on(), Exit::succeed(42));
  println!("006_effect_macro_binds ok");
}
