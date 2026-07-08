//! Build a capability [`Env`] with [`id_effect_logger::EffectLoggerLive`], then run an
//! effect that extracts `EffectLogger` via `~EffectLogger`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example layer_build`

use ::id_effect::{Effect, caps, effect, provide, run_with};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let prog: Effect<(), EffectLoggerError, caps!(EffectLogger)> = effect!(|r| {
    let logger = *~EffectLogger;
    ~logger.info::<caps!(EffectLogger)>("logger provided via EffectLoggerLive provider");
  });

  let result = run_with([provide!(EffectLoggerLive)], prog);
  println!("{result:?}");
}
