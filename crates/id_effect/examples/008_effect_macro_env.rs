//! Ex 008 — `effect!` with capability DI: `~Service` reads from the environment.

use id_effect::{Effect, caps, effect, provide, run_with};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Counter(pub i32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Counter, with = || Counter(41))]
struct CounterLive;

fn main() {
  let program: Effect<i32, (), caps!(Counter)> = effect!(|r| {
    let counter = ~Counter;
    counter.0 + 1
  });
  let n = run_with([provide!(CounterLive)], program).expect("run");
  assert_eq!(n, 42);
  println!("008_effect_macro_env ok");
}
