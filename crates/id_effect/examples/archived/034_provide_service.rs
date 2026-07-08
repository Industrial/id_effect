//! Ex 034 — conditional logic with multiple capabilities in one [`Env`].

use id_effect::{Effect, Env, ProviderSpec, caps, effect, provide, require, run_with, succeed};
struct Gate;
struct Value;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Gate)]
struct GateLive;

impl GateLive {
  fn new() -> bool {
    true
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Value)]
struct ValueLive;

impl ValueLive {
  fn new() -> i32 {
    42
  }
}

fn main() {
  let program: Effect<i32, (), caps!(Gate, Value)> = effect!(|_env: &mut caps!(Gate, Value)| {
    let on = ~succeed(*require!(Gate));
    let v = ~succeed(*require!(Value));
    if on { v } else { 0 }
  });
  let n = run_with([provide!(GateLive), provide!(ValueLive)], program).expect("run");
  assert_eq!(n, 42);
  println!("034_provide_service ok");
}
