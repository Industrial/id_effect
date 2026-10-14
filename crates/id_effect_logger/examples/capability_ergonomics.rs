//! A small feature module written with the 0.6 capability ergonomics — no clippy allows, no
//! turbofish, no `unreachable!`:
//!
//! - `type HelloCaps = caps!(…)` names the environment once and is usable everywhere
//!   (signature, `effect!` closure annotation, tests);
//! - `#[provides(Service, with = …)]` builds the service without a `fn new() -> Service`;
//! - `~logger.info(…)` needs no `::<R>` (log steps have no environment);
//! - `.widen_error()` lifts an infallible (`Never` or `()`) service error into the feature error.
//!
//! Run: `cargo run -p id_effect_logger --example capability_ergonomics`

use std::sync::Arc;

use id_effect::{Effect, Never, caps, effect, provide, run_with, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

/// Greeting operations exposed as effects.
trait HelloService: Send + Sync {
  fn greeting(&self) -> Effect<String, Never, caps!(Hello)>;
}

/// The capability: services are stored as cloneable handles.
type Hello = Arc<dyn HelloService>;

struct HelloServiceLive;

impl HelloService for HelloServiceLive {
  fn greeting(&self) -> Effect<String, Never, caps!(Hello)> {
    succeed("Hello, World!".to_string())
  }
}

/// Provider node: no `impl HelloLive { fn new() -> Hello }`, so no `clippy::new_ret_no_self`.
#[derive(id_effect::ProviderSpecDerive)]
#[provides(Hello, with = || Arc::new(HelloServiceLive) as Hello)]
struct HelloLive;

#[derive(Debug)]
enum HelloError {
  Log(EffectLoggerError),
}

impl std::fmt::Display for HelloError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Log(err) => write!(f, "hello: {err}"),
    }
  }
}

/// The feature's environment, named once.
type HelloCaps = caps!(Hello, EffectLogger);

fn hello() -> Effect<String, HelloError, HelloCaps> {
  effect!(|r: &mut HelloCaps| {
    let logger = *~EffectLogger;
    ~logger.info("running hello program").map_error(HelloError::Log);
    let hello = ~Hello;
    ~hello.greeting().widen_error()
  })
}

fn main() {
  let greeting = run_with([provide!(HelloLive), provide!(EffectLoggerLive)], hello())
    .unwrap_or_else(|e| panic!("hello failed: {e}"));
  assert_eq!(greeting, "Hello, World!");
  println!("capability_ergonomics ok: {greeting}");
}

#[cfg(test)]
mod tests {
  use super::*;

  struct HelloServiceMock;

  impl HelloService for HelloServiceMock {
    fn greeting(&self) -> Effect<String, Never, caps!(Hello)> {
      succeed("Hello, Mock!".to_string())
    }
  }

  #[derive(id_effect::ProviderSpecDerive)]
  #[provides(Hello, with = || Arc::new(HelloServiceMock) as Hello)]
  struct HelloMockLive;

  #[test]
  fn hello_uses_the_mock_provider() {
    let out = run_with(
      [provide!(HelloMockLive), provide!(EffectLoggerLive)],
      hello(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out, "Hello, Mock!");
  }
}
