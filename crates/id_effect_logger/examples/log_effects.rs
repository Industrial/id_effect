//! Extract [`EffectLogger`] once with `~EffectLoggerKey`, then call its methods
//! as `~logger.level(…)` steps — each returns `Effect<(), EffectLoggerError, R>`.
//!
//! Run: `RUST_LOG=trace devenv shell -- cargo run -p id_effect_logger --example log_effects`

use ::id_effect::{Effect, caps, effect, provide, run_with};
use id_effect_logger::{EffectLoggerError, EffectLoggerKey, EffectLoggerLive};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
    )
    .init();

  let prog: Effect<(), EffectLoggerError, caps!(EffectLoggerKey)> = effect!(|r| {
    let logger = *~EffectLoggerKey;
    ~logger.trace::<caps!(EffectLoggerKey)>("trace step");
    ~logger.debug::<caps!(EffectLoggerKey)>("debug step");
    ~logger.info::<caps!(EffectLoggerKey)>("info step");
    ~logger.warn::<caps!(EffectLoggerKey)>("warn step");
    ~logger.error::<caps!(EffectLoggerKey)>("error step");
  });

  run_with([provide!(EffectLoggerLive)], prog).expect("logging should not fail");
  println!("ran all five log levels via ~EffectLoggerKey");
}
