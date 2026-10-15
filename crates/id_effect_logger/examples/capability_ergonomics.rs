//! A small feature module written with the 0.7 capability ergonomics — no clippy
//! allows, no turbofish, no `unreachable!`:
//!
//! - `type HelloCaps = caps!(…)` names the environment once and is usable everywhere
//!   (signatures, tests);
//! - services are plain values put in the typed environment with `provide_env(hlist![…])`;
//! - `~logger.info(…)` needs no `::<R>` (log steps have no environment);
//! - `~hello.greeting()` binds a `Never`-typed service effect under the feature error.
//!
//! Run: `cargo run -p id_effect_logger --example capability_ergonomics`

use std::sync::Arc;

use id_effect::Never;
use id_effect::{Effect, caps, effect, hlist, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError};

/// Greeting operations exposed as effects.
trait HelloService: Send + Sync {
  fn greeting(&self) -> Effect<String, Never>;
}

/// The capability: services are stored as cloneable handles.
type Hello = Arc<dyn HelloService>;

struct HelloServiceLive;

impl HelloService for HelloServiceLive {
  fn greeting(&self) -> Effect<String, Never> {
    succeed("Hello, World!".to_string())
  }
}

#[derive(Debug, PartialEq)]
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
  effect!({
    let logger = ~EffectLogger;
    ~logger.info("running hello program").map_err(HelloError::Log);
    let hello = ~Hello;
    ~hello.greeting()
  })
}

fn main() {
  let live: Hello = Arc::new(HelloServiceLive);
  let greeting = hello()
    .provide_env(hlist![live, EffectLogger])
    .block_on()
    .into_result()
    .unwrap_or_else(|e| panic!("hello failed: {e:?}"));
  assert_eq!(greeting, "Hello, World!");
  println!("capability_ergonomics ok: {greeting}");
}

#[cfg(test)]
mod tests {
  use super::*;

  struct HelloServiceMock;

  impl HelloService for HelloServiceMock {
    fn greeting(&self) -> Effect<String, Never> {
      succeed("Hello, Mock!".to_string())
    }
  }

  #[test]
  fn hello_uses_the_mock_service() {
    let mock: Hello = Arc::new(HelloServiceMock);
    let out = hello()
      .provide_env(hlist![mock, EffectLogger])
      .block_on()
      .into_result()
      .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(out, "Hello, Mock!");
  }
}
