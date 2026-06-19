//! Provide [`EffectLogger`] via capability [`Env`], then extract it inside
//! `effect!` with `~EffectLoggerKey` and use its methods with `~`.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example context_service`

use ::id_effect::{Effect, caps, effect, provide, run_with};
use id_effect_logger::{EffectLoggerError, EffectLoggerKey, EffectLoggerLive};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let prog: Effect<(), EffectLoggerError, caps!(EffectLoggerKey)> = effect!(|r| {
    let logger = *~EffectLoggerKey;
    ~logger.info::<caps!(EffectLoggerKey)>("resolved EffectLogger via Env + EffectLoggerLive");
    ~logger.warn::<caps!(EffectLoggerKey)>("warn from the same extracted logger");
  });

  run_with([provide!(EffectLoggerLive)], prog)
    .expect("EffectLoggerError is never produced by tracing");
}
