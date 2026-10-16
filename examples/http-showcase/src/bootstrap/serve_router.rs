//! [`serve_router`]: the scoped HTTP server around one router.

use axum::Router;
use id_effect::{Effect, Layer, caps, effect, hlist};
use id_effect_axum::{HostConfig, HostError, ServerHandle, server_layer};

use super::log_startup;

/// Binds `app` on the [`HostConfig`] address ([`server_layer`]), logs the bound address and the
/// `store` kind, then waits for `stop`; leaving drains the server gracefully.
pub fn serve_router<A: Send + 'static>(
  app: Router,
  store: &'static str,
  stop: Effect<A, HostError, caps!(HostConfig)>,
) -> Effect<A, HostError, caps!(HostConfig)> {
  effect!(|_r: &caps!(HostConfig)| {
    // `stop` needs the HostConfig too: keep it next to the server's handle (provide_merge).
    let host: Layer<caps!(HostConfig), HostError> = Layer::succeed(hlist![HostConfig::clone(
      ~HostConfig
    )]);
    let serving = effect!(|_r: &caps!(ServerHandle, HostConfig)| {
      let addr = (~ServerHandle).local_addr();
      ~log_startup(addr, store);
      ~stop
    });
    ~serving.provide_layer(server_layer(app).provide_merge(host))
  })
}
