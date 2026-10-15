//! Log inside `effect!` using `~EffectLogger` and `~logger.level(…)` steps.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example macro_sync_bind`

use ::id_effect::Exit;
use ::id_effect::{Effect, HNil, caps, effect, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let program: Effect<i32, EffectLoggerError, caps!(EffectLogger)> = effect!({
    let logger = ~EffectLogger;
    ~logger.info("first step: log_info via ~logger");
    ~logger.warn("second step: log_warn via ~logger");
    let n = ~succeed::<i32, EffectLoggerError, HNil>(21);
    n + 1
  });

  let n = program.provide(EffectLogger).block_on();
  assert_eq!(n, Exit::Success(22));
  println!("macro_sync_bind ok: {n:?}");
}
