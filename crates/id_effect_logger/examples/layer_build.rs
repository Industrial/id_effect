//! Build a capability [`Env`] with [`id_effect_logger::EffectLoggerLive`], then run an
//! effect that extracts `EffectLogger` via `~EffectLogger`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example layer_build`

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
    ~logger.info("logger provided via EffectLoggerLive provider");
  });

  let result: Result<(), EffectLoggerError> = run_blocking(prog, logger_env());
  println!("{result:?}");
}
