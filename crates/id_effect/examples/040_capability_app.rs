#![allow(dead_code, clippy::new_ret_no_self)]

//! Capability DI: `#[capability]`, `ProviderSpec`, `run_with`.

use id_effect::{Effect, caps, effect, provide, run_with};

#[::id_effect::capability(Counter)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(CounterKey)]
struct CounterLive;

impl CounterLive {
  fn new() -> Counter {
    Counter(42)
  }
}

fn app() -> Effect<u32, (), caps!(CounterKey)> {
  effect!(|r| {
    let counter = ~CounterKey;
    counter.0
  })
}

fn main() {
  let n = run_with([provide!(CounterLive)], app()).expect("run");
  assert_eq!(n, 42);
  println!("counter = {n}");
}
