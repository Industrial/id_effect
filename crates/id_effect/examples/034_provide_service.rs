//! Ex 034 — conditional logic with multiple capabilities in one [`Env`].

use id_effect::{
  Effect, Env, ProviderError, ProviderSpec, define_capability, effect, provide, require, run_with,
  succeed,
};

define_capability!(GateKey, bool);
define_capability!(ValueKey, i32);

struct GateLive;
struct ValueLive;

impl ProviderSpec for GateLive {
  type Key = GateKey;
  type Output = bool;

  fn provider_id() -> &'static str {
    "gate-live"
  }

  fn provide(_deps: &Env) -> Result<bool, ProviderError> {
    Ok(true)
  }
}

impl ProviderSpec for ValueLive {
  type Key = ValueKey;
  type Output = i32;

  fn provider_id() -> &'static str {
    "value-live"
  }

  fn provide(_deps: &Env) -> Result<i32, ProviderError> {
    Ok(42)
  }
}

fn main() {
  let program: Effect<i32, (), Env> = effect!(|env: &mut Env| {
    let on = ~succeed(*require!(env, GateKey));
    let v = ~succeed(*require!(env, ValueKey));
    if on { v } else { 0 }
  });
  let n = run_with([provide!(GateLive), provide!(ValueLive)], program).expect("run");
  assert_eq!(n, 42);
  println!("034_provide_service ok");
}
