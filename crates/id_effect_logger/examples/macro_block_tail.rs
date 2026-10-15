//! Mix `~` effect steps with ordinary Rust control flow inside `effect!`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example macro_block_tail`

use ::id_effect::Exit;
use ::id_effect::{Effect, HNil, caps, effect, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let program: Effect<i32, EffectLoggerError, caps!(EffectLogger)> = effect!({
    let logger = ~EffectLogger;
    let seed = ~succeed::<i32, EffectLoggerError, HNil>(6);
    if seed > 0 {
      ~logger.info("seed is positive");
      seed * 2
    } else {
      0
    }
  });

  let n = program.provide(EffectLogger).block_on();
  assert_eq!(n, Exit::Success(12));
  println!("macro_block_tail ok: {n:?}");
}
