#![allow(dead_code, clippy::new_ret_no_self)]

//! Ex 008 — `effect!` with capability DI: `~Key` reads from the environment.

use id_effect::{Effect, caps, effect, provide, run_with};

#[::id_effect::capability(i32)]
struct Counter;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(CounterKey)]
struct CounterLive;

impl CounterLive {
  fn new() -> i32 {
    41
  }
}

fn main() {
  let program: Effect<i32, (), caps!(CounterKey)> = effect!(|r| {
    let n = *~CounterKey;
    n + 1
  });
  let n = run_with([provide!(CounterLive)], program).expect("run");
  assert_eq!(n, 42);
  println!("008_effect_macro_env ok");
}
