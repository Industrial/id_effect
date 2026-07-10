//! Log inside `effect!` using `~EffectLogger` and `~logger.level(…)` steps.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example macro_sync_bind`

use ::id_effect::{Effect, Env, caps, effect, provide, run_with, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let program: Effect<i32, EffectLoggerError, caps!(EffectLogger)> = effect!(|r| {
    let logger = *~EffectLogger;
    ~logger.info::<caps!(EffectLogger)>("first step: log_info via ~logger");
    ~logger.warn::<caps!(EffectLogger)>("second step: log_warn via ~logger");
    let n = ~succeed::<i32, EffectLoggerError, Env>(21);
    n + 1
  });

  let n = run_with([provide!(EffectLoggerLive)], program).expect("run");
  assert_eq!(n, 22);
  println!("macro_sync_bind ok: {n}");
}
