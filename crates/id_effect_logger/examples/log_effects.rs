//! Extract [`EffectLogger`] once with `~EffectLogger`, then call its methods
//! as `~logger.level(…)` steps — each returns `Effect<(), EffectLoggerError>`.
//!
//! Run: `RUST_LOG=trace devenv shell -- cargo run -p id_effect_logger --example log_effects`

use ::id_effect::Exit;
use ::id_effect::{Effect, caps, effect};
use id_effect_logger::{EffectLogger, EffectLoggerError};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
    )
    .init();

  let prog: Effect<(), EffectLoggerError, caps!(EffectLogger)> = effect!({
    let logger = ~EffectLogger;
    ~logger.trace("trace step");
    ~logger.debug("debug step");
    ~logger.info("info step");
    ~logger.warn("warn step");
    ~logger.error("error step");
  });

  assert_eq!(prog.provide(EffectLogger).block_on(), Exit::Success(()));
  println!("ran all five log levels via ~EffectLogger");
}
