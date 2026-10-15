//! Capability DI on the 0.7 kernel: a typed `R`, a provider derive building a `Layer`, and
//! `provide_layer` at the edge.
use id_effect::next::{Effect, caps_next, effect_next};
use id_effect::{Exit, Never};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Counter, with = || Counter(42))]
struct CounterLive;

fn app() -> Effect<u32, Never, caps_next!(Counter)> {
  effect_next!({ ~Counter.0 })
}

fn main() {
  let exit = app().provide_layer(CounterLive::layer()).block_on();
  assert_eq!(exit, Exit::succeed(42));
  println!("counter = 42");
}
