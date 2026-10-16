//! [`run_until_shutdown`]: the service until SIGINT / SIGTERM.

use id_effect::{Effect, caps, from_async};
use id_effect_axum::{ShutdownReason, wait_for_shutdown};

use super::{StartupError, run_until};
use crate::infrastructure::config::AppConfig;

/// [`run_until`] SIGINT / SIGTERM; returns which one stopped it.
pub fn run_until_shutdown() -> Effect<ShutdownReason, StartupError, caps!(AppConfig)> {
  run_until(from_async(|| async { Ok(wait_for_shutdown().await) }))
}
