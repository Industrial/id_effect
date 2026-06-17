//! Capability DI v2: `define_capability!`, `ProviderSpec`, `run_with`.

use id_effect::{
  Effect, Env, ProviderError, ProviderSpec, define_capability, provide, require, run_with,
};

define_capability!(CounterKey, Counter);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

struct CounterLive;

impl ProviderSpec for CounterLive {
  type Key = CounterKey;
  type Output = Counter;

  fn provider_id() -> &'static str {
    "counter-live"
  }

  fn provide(_deps: &Env) -> Result<Counter, ProviderError> {
    Ok(Counter(42))
  }
}

fn app() -> Effect<u32, (), id_effect::Env> {
  Effect::new(|env: &mut id_effect::Env| {
    let counter = require!(env, CounterKey);
    Ok(counter.0)
  })
}

fn main() {
  let n = run_with([provide!(CounterLive)], app()).expect("run");
  assert_eq!(n, 42);
  println!("counter = {n}");
}
