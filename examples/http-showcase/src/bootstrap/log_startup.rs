//! [`log_startup`]: the one line the service logs once it listens.

use std::net::SocketAddr;

use id_effect::{Effect, succeed};
use id_effect_logger::{EffectLogger, annotate_logs};

use super::logging_layer;

/// Logs `listening` with the bound `addr` and the `store` kind (`memory` or `postgres`) through
/// the [`logging_layer`]. A logging failure never stops the service.
pub fn log_startup<E: Send + 'static>(addr: SocketAddr, store: &'static str) -> Effect<(), E> {
  annotate_logs(
    "addr",
    addr.to_string(),
    annotate_logs("store", store, EffectLogger.info("http_showcase listening")),
  )
  .catch(|_| succeed(()))
  .provide_layer(logging_layer())
}
