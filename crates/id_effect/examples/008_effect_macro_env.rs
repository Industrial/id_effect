//! Ex 008 — `effect_next!` reads a capability with `~Service`; `provide` supplies it.
use id_effect::next::{Effect, caps_next, effect_next};
use id_effect::{Exit, Never};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Counter(pub i32);

fn program() -> Effect<i32, Never, caps_next!(Counter)> {
  effect_next!({ ~Counter.0 + 1 })
}

fn main() {
  assert_eq!(program().provide(Counter(41)).block_on(), Exit::succeed(42));
  println!("008_effect_macro_env ok");
}
