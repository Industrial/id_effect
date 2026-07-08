//! Mix `~` effect steps with ordinary Rust control flow inside `effect!`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example macro_block_tail`

use ::id_effect::{Effect, Env, caps, effect, provide, run_with, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let program: Effect<i32, EffectLoggerError, caps!(EffectLogger)> = effect!(|r| {
    let logger = *~EffectLogger;
    let seed = ~succeed::<i32, EffectLoggerError, Env>(6);
    if seed > 0 {
      ~logger.info::<caps!(EffectLogger)>("seed is positive");
      seed * 2
    } else {
      0
    }
  });

  let n = run_with([provide!(EffectLoggerLive)], program).expect("run");
  assert_eq!(n, 12);
  println!("macro_block_tail ok: {n}");
}
