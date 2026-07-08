//! Extract [`EffectLogger`] once with `~EffectLogger`, then call its methods
//! as `~logger.level(…)` steps — each returns `Effect<(), EffectLoggerError, R>`.
//!
//! Run: `RUST_LOG=trace devenv shell -- cargo run -p id_effect_logger --example log_effects`

use ::id_effect::{Effect, caps, effect, provide, run_with};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
    )
    .init();

  let prog: Effect<(), EffectLoggerError, caps!(EffectLogger)> = effect!(|r| {
    let logger = *~EffectLogger;
    ~logger.trace::<caps!(EffectLogger)>("trace step");
    ~logger.debug::<caps!(EffectLogger)>("debug step");
    ~logger.info::<caps!(EffectLogger)>("info step");
    ~logger.warn::<caps!(EffectLogger)>("warn step");
    ~logger.error::<caps!(EffectLogger)>("error step");
  });

  run_with([provide!(EffectLoggerLive)], prog).expect("logging should not fail");
  println!("ran all five log levels via ~EffectLogger");
}
