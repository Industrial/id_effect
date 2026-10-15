//! [`EffectLogger`] as a service value: each method returns an
//! `Effect<(), EffectLoggerError>` (no environment) that is run at the edge with `block_on`
//! (the logger is self-contained after extraction).
//!
//! Run: `RUST_LOG=debug devenv shell -- cargo run -p id_effect_logger --example effect_logger`

use ::id_effect::Exit;
use id_effect_logger::{EffectLogger, LogLevel};

fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace")),
    )
    .init();

  let log = EffectLogger;
  for step in [
    log.trace("trace line (raise RUST_LOG=trace to see)"),
    log.debug("debug line (raise RUST_LOG=debug to see)"),
    log.info("info: EffectLogger methods work"),
    log.warn("warn example"),
    log.error("error example"),
    // .log() also accepts an explicit LogLevel:
    log.log(LogLevel::Info, "explicit level via .log()"),
  ] {
    assert_eq!(step.block_on(), Exit::Success(()));
  }
}
