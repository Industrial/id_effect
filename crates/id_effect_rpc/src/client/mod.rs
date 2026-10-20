//! The Rust RPC client (feature `client`): call remote procedures as effects
//! ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) `RpcClient` parity).
//!
//! [`RpcClient`] holds where and how to call; the transports implement the calls: [`http`]
//! (`POST /rpc` through id_effect_platform's `HttpClientService`), `ws` (feature `ws`: every
//! kind multiplexed on one WebSocket) and `sse` (feature `sse`: subscriptions and server streams
//! as server-sent events, through the same `HttpClientService`).
//!
//! [`RpcClientConfig::transport`] is the choice: [`RpcClient::subscribe`] opens a stream over
//! the configured [`TransportKind`] and yields [`CallEvent`]s whichever it is.
//!
//! ```rust
//! use id_effect_rpc::client::{RpcClient, RpcClientConfig, TransportKind};
//!
//! let config = RpcClientConfig::new("https://api.example.com").with_transport(TransportKind::WebSocket);
//! assert_eq!(config.ws_url(), "wss://api.example.com/rpc/ws");
//! assert_eq!(config.tag_url("Chat.room"), "https://api.example.com/rpc/Chat.room");
//! assert_eq!(RpcClient::with_config(config).transport(), TransportKind::WebSocket);
//! ```

pub mod http;
// r12-e12-2-mesh-rpc: calls to a mesh endpoint (feature `mesh`).
#[cfg(feature = "mesh")]
pub mod mesh;
#[cfg(feature = "sse")]
pub mod sse;
#[cfg(feature = "ws")]
pub mod ws;

use id_effect::{Effect, HList, Selector, fail};
use id_effect_axum::route::Problem;
use id_effect_platform::http::HttpClientService;
use serde_json::Value;

use crate::protocol::RPC_DISPATCH_PATH;
use crate::serialization::RpcSerializationError;
use crate::wire::{Cursor, RpcFailure, WireExit};

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
  /// The stream transport failed: the connection closed or broke the frame protocol, or the
  /// transport is not built or does not serve the call.
  #[error("transport: {0}")]
  Transport(String),
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

/// Which transport carries a client's streams ([`RpcClientConfig::transport`]). Unary calls go
/// over HTTP whatever it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TransportKind {
  /// HTTP only: NDJSON streams through `POST /rpc/{tag}` (default).
  #[default]
  Http,
  /// A WebSocket (feature `ws`, `client::ws::WsClient`), every kind multiplexed on it.
  WebSocket,
  /// Server-sent events on `GET /rpc/{tag}` (feature `sse`): subscriptions and server streams.
  Sse,
}

/// Configuration for [`RpcClient`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcClientConfig {
  /// Base URL (e.g. `http://127.0.0.1:8080`).
  pub base_url: String,
  /// Dispatch path (default [`RPC_DISPATCH_PATH`]).
  pub dispatch_path: String,
  /// The stream transport (default [`TransportKind::Http`]).
  pub transport: TransportKind,
}

impl RpcClientConfig {
  /// New config with the default `/rpc` path.
  #[inline]
  pub fn new(base_url: impl Into<String>) -> Self {
    Self {
      base_url: base_url.into(),
      dispatch_path: RPC_DISPATCH_PATH.to_owned(),
      transport: TransportKind::Http,
    }
  }

  /// Carry streams over `transport`.
  #[must_use]
  pub fn with_transport(mut self, transport: TransportKind) -> Self {
    self.transport = transport;
    self
  }

  /// The dispatch URL.
  pub fn url(&self) -> String {
    let base = self.base_url.trim_end_matches('/');
    format!("{base}{}", self.dispatch_path)
  }

  /// The URL of procedure `tag` (`{url}/{tag}`: per-procedure calls, server-sent events).
  pub fn tag_url(&self, tag: &str) -> String {
    format!("{}/{tag}", self.url().trim_end_matches('/'))
  }

  /// The WebSocket URL (`{url}/ws`, `http` → `ws`, `https` → `wss`).
  pub fn ws_url(&self) -> String {
    let url = self.url();
    let url = match url.split_once("://") {
      Some(("https", rest)) => format!("wss://{rest}"),
      Some(("http", rest)) => format!("ws://{rest}"),
      _ => url,
    };
    format!("{}/ws", url.trim_end_matches('/'))
  }
}

/// One event of a stream call, on any transport.
#[derive(Debug)]
pub enum CallEvent {
  /// One item, with the cursor to resume after (subscriptions).
  Item {
    /// The item.
    value: Value,
    /// Its resume position, if any.
    cursor: Option<Cursor>,
  },
  /// How the call ended: its output, or its failure (typed or reserved, or the transport's).
  Exit(Result<Value, RpcClientError>),
}

/// The [`WireExit`] JSON of an `exit` frame or event as a result.
pub fn exit_result(exit: Option<Value>) -> Result<Value, RpcClientError> {
  match exit.map(serde_json::from_value::<WireExit>) {
    Some(Ok(WireExit::Success { value })) => Ok(value),
    Some(Ok(WireExit::Failure { error })) => Err(RpcClientError::Failure(error)),
    _ => Err(RpcClientError::Transport("malformed exit".to_owned())),
  }
}

/// An open stream call over the configured transport ([`RpcClient::subscribe`]).
#[non_exhaustive]
pub enum RpcStream {
  /// Over a WebSocket.
  #[cfg(feature = "ws")]
  WebSocket(std::sync::Arc<tokio::sync::Mutex<ws::WsCall>>),
  /// Over server-sent events.
  #[cfg(feature = "sse")]
  Sse(sse::SseCall),
}

impl std::fmt::Debug for RpcStream {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("RpcStream(..)")
  }
}

impl RpcStream {
  /// The next event: items, then one [`CallEvent::Exit`], then `None`.
  pub fn next(&self) -> Effect<Option<CallEvent>, RpcClientError> {
    match self {
      #[cfg(feature = "ws")]
      Self::WebSocket(call) => {
        let call = std::sync::Arc::clone(call);
        id_effect::from_async(move || async move { Ok(call.lock().await.next().await) })
      }
      #[cfg(feature = "sse")]
      Self::Sse(call) => call.next(),
      #[allow(unreachable_patterns)]
      _ => id_effect::succeed(None),
    }
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

  /// Carry streams over `transport`.
  #[must_use]
  pub fn with_transport(mut self, transport: TransportKind) -> Self {
    self.config.transport = transport;
    self
  }

  /// The configured stream transport.
  pub fn transport(&self) -> TransportKind {
    self.config.transport
  }

  /// Open the subscription or server stream `tag` with `input` over the configured transport,
  /// resuming a subscription after `after`: a fresh WebSocket for [`TransportKind::WebSocket`]
  /// (open a `ws::WsClient` to multiplex several), server-sent events through the
  /// `HttpClientService` for [`TransportKind::Sse`]. [`TransportKind::Http`], or a transport whose
  /// feature is off, fails with [`RpcClientError::Transport`].
  pub fn subscribe<R, I>(
    &self,
    tag: impl Into<String>,
    input: Value,
    after: Option<Cursor>,
  ) -> Effect<RpcStream, RpcClientError, R>
  where
    R: Selector<HttpClientService, I> + HList + Send + Sync + 'static,
  {
    let tag: String = tag.into();
    #[cfg(not(any(feature = "ws", feature = "sse")))]
    let _ = (tag, input, after);
    match self.config.transport {
      #[cfg(feature = "ws")]
      TransportKind::WebSocket => {
        let url = self.config.ws_url();
        id_effect::from_async(move || async move {
          let client = ws::WsClient::connect(&url).await?;
          let call = client.open(&tag, input, after).await?;
          Ok(RpcStream::WebSocket(std::sync::Arc::new(
            tokio::sync::Mutex::new(call),
          )))
        })
      }
      #[cfg(feature = "sse")]
      TransportKind::Sse => self.sse(&tag, &input, after).map(RpcStream::Sse),
      other => fail(RpcClientError::Transport(format!(
        "streams are not carried over {other:?} by this build (features `ws`, `sse`)"
      ))),
    }
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
  fn transports_and_their_urls() {
    let config = RpcClientConfig::new("http://h:1/");
    assert_eq!(config.transport, TransportKind::Http);
    let ws = config.clone().with_transport(TransportKind::WebSocket);
    assert_eq!(ws.transport, TransportKind::WebSocket);
    assert_eq!(config.ws_url(), "ws://h:1/rpc/ws");
    assert_eq!(RpcClientConfig::new("h").ws_url(), "h/rpc/ws");
    let client = RpcClient::new("https://h").with_transport(TransportKind::Sse);
    assert_eq!(client.transport(), TransportKind::Sse);
    assert_eq!(client.config().ws_url(), "wss://h/rpc/ws");
    assert_eq!(client.config().tag_url("A.b"), "https://h/rpc/A.b");
    assert_eq!(
      exit_result(Some(serde_json::json!({"_tag": "Success", "value": 1}))).unwrap(),
      1
    );
    let failure = exit_result(Some(
      serde_json::json!({"_tag": "Failure", "error": {"_tag": "X"}}),
    ));
    assert_eq!(failure.unwrap_err().failure_tag(), Some("X"));
    for bad in [None, Some(serde_json::json!(1))] {
      let err = exit_result(bad).unwrap_err();
      assert_eq!(err.to_string(), "transport: malformed exit");
    }
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
