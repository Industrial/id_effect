//! Ex 008 — `effect!` with capability DI: `require!` reads from [`Env`].

use id_effect::{
  Effect, Env, ProviderError, ProviderSpec, define_capability, effect, provide, require, run_with,
  succeed,
};

define_capability!(CounterKey, i32);

struct CounterLive;

impl ProviderSpec for CounterLive {
  type Key = CounterKey;
  type Output = i32;

  fn provider_id() -> &'static str {
    "counter-live"
  }

  fn provide(_deps: &Env) -> Result<i32, ProviderError> {
    Ok(41)
  }
}

fn main() {
  let program: Effect<i32, (), Env> = effect!(|env: &mut Env| {
    let n = ~succeed(*require!(env, CounterKey));
    n + 1
  });
  let n = run_with([provide!(CounterLive)], program).expect("run");
  assert_eq!(n, 42);
  println!("008_effect_macro_env ok");
}
