//! The Rust RPC client (feature `client`): call remote procedures as effects
//! ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) `RpcClient` parity).
//!
//! [`RpcClient`] holds where and how to call; the transports implement the calls: [`http`]
//! (`POST /rpc` through id_effect_platform's `HttpClientService`) today, WebSocket and SSE with
//! features `ws` / `sse`.

pub mod http;

use crate::envelope::RpcEnvelope;
use crate::protocol::RPC_DISPATCH_PATH;
use crate::serialization::RpcSerializationError;

/// Header every first-party client sends on calls (`x-rpc: 1`, ADR 0014 C5).
pub const RPC_HEADER: &str = "x-rpc";

/// Client-side RPC failures: the wire, the transport, or the remote procedure's failure.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
  /// Wire encode/decode or schema validation failed.
  #[error("serialization: {0}")]
  Serialization(#[from] RpcSerializationError),
  /// Platform HTTP transport error.
  #[error("http: {0}")]
  Http(#[from] id_effect_platform::error::HttpError),
  /// The remote procedure failed with this envelope.
  #[error("rpc {code:?}: {message}", code = .0.code, message = .0.message)]
  Rpc(RpcEnvelope),
  /// The server refused the call before running it (a Problem: unknown procedure, guard,
  /// codec, size), or answered with an unexpected status.
  #[error("unexpected status {0}")]
  UnexpectedStatus(u16),
}

/// Configuration for [`RpcClient`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcClientConfig {
  /// Base URL (e.g. `http://127.0.0.1:8080`).
  pub base_url: String,
  /// Dispatch path (default [`RPC_DISPATCH_PATH`]).
  pub dispatch_path: String,
}

impl RpcClientConfig {
  /// New config with the default `/rpc` path.
  #[inline]
  pub fn new(base_url: impl Into<String>) -> Self {
    Self {
      base_url: base_url.into(),
      dispatch_path: RPC_DISPATCH_PATH.to_owned(),
    }
  }

  /// The dispatch URL.
  pub fn url(&self) -> String {
    let base = self.base_url.trim_end_matches('/');
    format!("{base}{}", self.dispatch_path)
  }
}

/// Typed RPC client.
#[derive(Clone, Debug)]
pub struct RpcClient {
  config: RpcClientConfig,
}

impl RpcClient {
  /// A client for `base_url`.
  #[inline]
  pub fn new(base_url: impl Into<String>) -> Self {
    Self::with_config(RpcClientConfig::new(base_url))
  }

  /// A client for `config`.
  pub fn with_config(config: RpcClientConfig) -> Self {
    Self { config }
  }

  /// Override the dispatch path (default `/rpc`).
  #[must_use]
  pub fn with_dispatch_path(mut self, path: impl Into<String>) -> Self {
    self.config.dispatch_path = path.into();
    self
  }

  /// The configuration.
  pub fn config(&self) -> &RpcClientConfig {
    &self.config
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::envelope::RpcErrorCode;

  #[test]
  fn urls_join_base_and_path() {
    let client = RpcClient::new("http://h:1/").with_dispatch_path("/api/rpc");
    assert_eq!(client.config().url(), "http://h:1/api/rpc");
    assert_eq!(RpcClientConfig::new("http://h").url(), "http://h/rpc");
    assert_eq!(
      RpcClient::with_config(RpcClientConfig::new("x")).config(),
      &RpcClientConfig::new("x")
    );
  }

  #[test]
  fn errors_display_their_cause() {
    let rpc = RpcClientError::Rpc(RpcEnvelope {
      code: RpcErrorCode::NotFound,
      message: "no user".into(),
      correlation_id: None,
      details: None,
    });
    assert_eq!(rpc.to_string(), "rpc NotFound: no user");
    assert_eq!(
      RpcClientError::UnexpectedStatus(404).to_string(),
      "unexpected status 404"
    );
    let ser: RpcClientError = RpcSerializationError::Protocol("p".into()).into();
    assert!(ser.to_string().starts_with("serialization"));
    let http: RpcClientError =
      id_effect_platform::error::HttpError::InvalidRequest("bad url".into()).into();
    assert!(http.to_string().starts_with("http"));
  }
}
