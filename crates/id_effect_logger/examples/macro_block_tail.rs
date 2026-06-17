//! Mix `~` effect steps with ordinary Rust control flow inside `effect!`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example macro_block_tail`

use ::id_effect::{Effect, Env, build_env, effect, provide, run_blocking, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn logger_env() -> Env {
  build_env([provide!(EffectLoggerLive)]).expect("EffectLoggerLive is infallible")
}

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let program: Effect<i32, EffectLoggerError, Env> = effect!(|_r: &mut Env| {
    let logger = ~EffectLogger;
    let seed = ~succeed::<i32, EffectLoggerError, Env>(6);
    if seed > 0 {
      ~logger.info("seed is positive");
      seed * 2
    } else {
      0
    }
  });

  let n = run_blocking(program, logger_env()).expect("run");
  assert_eq!(n, 12);
  println!("macro_block_tail ok: {n}");
}
