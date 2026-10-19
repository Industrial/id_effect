//! The Rust RPC client (feature `client`): call remote procedures as effects
//! ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) `RpcClient` parity).
//!
//! [`RpcClient`] holds where and how to call; the transports implement the calls: [`http`]
//! (`POST /rpc` through id_effect_platform's `HttpClientService`) today, WebSocket and SSE with
//! features `ws` / `sse`.

pub mod http;

use id_effect_axum::route::Problem;
use serde_json::Value;

use crate::protocol::RPC_DISPATCH_PATH;
use crate::serialization::RpcSerializationError;
use crate::wire::RpcFailure;

pub use crate::server::RPC_HEADER;

/// Client-side RPC failures: the wire, the transport, the server's refusal, or the remote
/// procedure's failure.
#[derive(Debug, thiserror::Error)]
pub enum RpcClientError {
  /// Wire encode/decode or schema validation failed.
  #[error("serialization: {0}")]
  Serialization(#[from] RpcSerializationError),
  /// Platform HTTP transport error.
  #[error("http: {0}")]
  Http(#[from] id_effect_platform::error::HttpError),
  /// The procedure ran and failed: its typed error (`{"_tag": …}`, a member of the procedure's
  /// error union) or a reserved failure ([`RpcClientError::reserved`]).
  #[error("rpc failure {}", failure_tag(.0))]
  Failure(Value),
  /// The server refused the call before running it: an RFC 9457 Problem (unknown procedure,
  /// guard, codec, size).
  #[error("refused {}: {}", .0.status.as_u16(), .0.detail.as_deref().unwrap_or(&.0.title))]
  Refused(Box<Problem>),
  /// Any other response.
  #[error("unexpected status {0}")]
  UnexpectedStatus(u16),
}

fn failure_tag(error: &Value) -> &str {
  error
    .get("_tag")
    .and_then(Value::as_str)
    .unwrap_or("(untagged)")
}

impl RpcClientError {
  /// The `_tag` of a [`RpcClientError::Failure`].
  pub fn failure_tag(&self) -> Option<&str> {
    match self {
      Self::Failure(error) => Some(failure_tag(error)),
      _ => None,
    }
  }

  /// The reserved failure (`InternalError`, `InvalidInput`, …) of a
  /// [`RpcClientError::Failure`], if it is one.
  pub fn reserved(&self) -> Option<RpcFailure> {
    match self {
      Self::Failure(error) if RpcFailure::is_reserved(failure_tag(error)) => {
        serde_json::from_value(error.clone()).ok()
      }
      _ => None,
    }
  }
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
  use axum::http::StatusCode;

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
    let rpc = RpcClientError::Failure(serde_json::json!({"_tag": "NotFound"}));
    assert_eq!(rpc.to_string(), "rpc failure NotFound");
    assert_eq!(rpc.failure_tag(), Some("NotFound"));
    assert!(rpc.reserved().is_none());
    let reserved = RpcClientError::Failure(serde_json::json!({"_tag": "DeadlineExceeded"}));
    assert_eq!(reserved.reserved(), Some(RpcFailure::DeadlineExceeded));
    let untagged = RpcClientError::Failure(serde_json::json!(1));
    assert_eq!(untagged.to_string(), "rpc failure (untagged)");
    assert!(untagged.reserved().is_none());
    let refused = RpcClientError::Refused(Box::new(Problem::new(StatusCode::FORBIDDEN)));
    assert_eq!(refused.to_string(), "refused 403: Forbidden");
    assert!(refused.failure_tag().is_none() && refused.reserved().is_none());
    let detailed = Problem::new(StatusCode::NOT_FOUND).with_detail("unknown rpc procedure");
    assert_eq!(
      RpcClientError::Refused(Box::new(detailed)).to_string(),
      "refused 404: unknown rpc procedure"
    );
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
