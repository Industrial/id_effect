//! Log inside `effect!` using `~EffectLogger` extraction and `~logger.level(…)` steps.
//!
//! Run: `devenv shell -- cargo run -p id_effect_logger --example macro_sync_bind`

use ::id_effect::{Effect, Env, build_env, effect, provide, run_blocking, succeed};
use id_effect_logger::{EffectLogger, EffectLoggerError, EffectLoggerLive};

fn logger_env() -> Env {
  build_env([provide!(EffectLoggerLive)]).expect("EffectLoggerLive is infallible")
}

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
    .init();

  let program: Effect<i32, EffectLoggerError, Env> = effect!(|_r: &mut Env| {
    let logger = ~EffectLogger;
    ~logger.info("first step: log_info via ~logger");
    ~logger.warn("second step: log_warn via ~logger");
    let n = ~succeed::<i32, EffectLoggerError, Env>(21);
    n + 1
  });

  let n = run_blocking(program, logger_env()).expect("run");
  assert_eq!(n, 22);
  println!("macro_sync_bind ok: {n}");
}
