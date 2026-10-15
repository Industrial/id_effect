//! Provide [`EffectLogger`] in a typed `next` environment, then extract it inside
//! `effect_next!` with `~EffectLogger` and use its methods with `~`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example context_service`

use ::id_effect::Exit;
use ::id_effect::next::{Effect, caps_next, effect_next};
use id_effect_logger::{EffectLogger, EffectLoggerError};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let prog: Effect<(), EffectLoggerError, caps_next!(EffectLogger)> = effect_next!({
    let logger = ~EffectLogger;
    ~logger.info("resolved EffectLogger from the typed environment");
    ~logger.warn("warn from the same extracted logger");
  });

  assert_eq!(prog.provide(EffectLogger).block_on(), Exit::Success(()));
}
