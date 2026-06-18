//! Ex 034 — conditional logic with multiple capabilities in one [`Env`].

use id_effect::{Effect, Env, ProviderSpec, caps, effect, provide, require, run_with, succeed};

#[::id_effect::capability(bool)]
struct Gate;

#[::id_effect::capability(i32)]
struct Value;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(GateKey)]
struct GateLive;

impl GateLive {
  fn new() -> bool {
    true
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(ValueKey)]
struct ValueLive;

impl ValueLive {
  fn new() -> i32 {
    42
  }
}

fn main() {
  let program: Effect<i32, (), caps!(GateKey, ValueKey)> = effect!(|_env: &mut caps!(GateKey, ValueKey)| {
    let on = ~succeed(*require!(GateKey));
    let v = ~succeed(*require!(ValueKey));
    if on { v } else { 0 }
  });
  let n = run_with([provide!(GateLive), provide!(ValueLive)], program).expect("run");
  assert_eq!(n, 42);
  println!("034_provide_service ok");
}
