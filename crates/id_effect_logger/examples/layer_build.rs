//! Build the [`EffectLogger`] capability from a [`Layer`], then run an effect that
//! extracts it via `~EffectLogger`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example layer_build`

use ::id_effect::{Effect, HCons, HNil, Layer, caps, effect, hlist};
use id_effect_logger::{EffectLogger, EffectLoggerError};

fn logger_layer() -> Layer<HCons<EffectLogger, HNil>, EffectLoggerError> {
  Layer::succeed(hlist![EffectLogger])
}

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let prog: Effect<(), EffectLoggerError, caps!(EffectLogger)> = effect!({
    let logger = ~EffectLogger;
    ~logger.info("logger provided via a Layer");
  });

  let result = prog.provide_layer(logger_layer()).block_on();
  println!("{result:?}");
}
