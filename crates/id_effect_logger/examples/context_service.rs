//! Provide [`EffectLogger`] via capability [`Env`], then extract it inside
//! `effect!` with `~EffectLogger` and use its methods with `~`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example context_service`

use ::id_effect::{Effect, Env, build_env, effect, provide, run_blocking};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn logger_env() -> Env {
  build_env([provide!(EffectLoggerLive)]).expect("EffectLoggerLive is infallible")
}

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let prog: Effect<(), EffectLoggerError, Env> = effect!(|_r: &mut Env| {
    let logger = ~EffectLogger;
    ~logger.info("resolved EffectLogger via Env + EffectLoggerLive");
    ~logger.warn("warn from the same extracted logger");
  });

  run_blocking(prog, logger_env()).expect("EffectLoggerError is never produced by tracing");
}
