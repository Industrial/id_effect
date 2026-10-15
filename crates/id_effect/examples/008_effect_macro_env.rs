//! Ex 008 — `effect!` reads a capability with `~Service`; `provide` supplies it.
use id_effect::{Effect, caps, effect};
use id_effect::{Exit, Never};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Counter(pub i32);

fn program() -> Effect<i32, Never, caps!(Counter)> {
  effect!({ ~Counter.0 + 1 })
}

fn main() {
  assert_eq!(program().provide(Counter(41)).block_on(), Exit::succeed(42));
  println!("008_effect_macro_env ok");
}
