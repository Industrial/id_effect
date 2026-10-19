//! The transports of an [`RpcGroup`](crate::RpcGroup) besides the HTTP routes of
//! [`RpcServer`](crate::RpcServer). Every transport runs its calls through
//! [`dispatch`](crate::server::dispatch), so guards, decoding, typed errors, deadlines and
//! cancellation are those of the HTTP routes.
//!
//! | Module | Feature | What |
//! |---|---|---|
//! | `mesh` | `mesh` | the same contract node to node over the process mesh (postcard always) |
//! | [`frame`] | — | the frame codec ([`frame::decode`], [`frame::encode`]) every frame transport shares |
//! | `ws` | `ws` | `GET {path}/ws`: one WebSocket, many calls multiplexed by call id, credit backpressure |
//! | `sse` | `sse` | `GET {path}/{tag}` with `Accept: text/event-stream`: subscriptions and server streams |
//! | `webtransport` | `webtransport` | `CONNECT {path}/wt` on HTTP/3: the WebSocket transport's connection logic, one QUIC stream per call |
//!
//! The per-connection limits of the WebSocket and SSE transports (connections per client IP and
//! per endpoint, frame size, message rate, heartbeats, idle timeout) are those of
//! `id_effect_axum`'s realtime layer (ADR 0014 C3); the WebSocket transport adds a cap on the calls
//! open on one connection. Their calls run with the request's capabilities and a
//! [`ConnectionId`].
//!
//! A call refused before it ran — an unknown tag or version, a guard's refusal — is answered on a
//! frame transport with an `exit` frame whose failure is [`REJECTED_TAG`] (see
//! [`rejection_outcome`]): HTTP answers those with a Problem, a multiplexed connection cannot.

// r12-e12-2-mesh-rpc: RPC endpoints as mesh processes under a cluster-wide name.
#[cfg(feature = "mesh")]
pub mod mesh;

// r12-e12-2-rpc-ws-sse: the frame codec, WebSocket (`ws`) and SSE (`sse`) transports.
pub mod frame;
#[cfg(feature = "sse")]
pub mod sse;
#[cfg(feature = "ws")]
pub mod ws;
// r12-e12-3-webtransport: every kind over WebTransport sessions, one QUIC stream per call.
#[cfg(feature = "webtransport")]
pub mod webtransport;

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::handler::CallOutcome;
use crate::server::dispatch::Rejection;

/// The connection a call arrived on, in every call's environment: one per WebSocket (every call
/// on the socket shares it) and one per server-sent-events stream. Channels count topics per
/// connection by it and show it as a member's presence `conn` (`channels::ConnectionId` is this
/// type). Random, so it names the connection without revealing its resume session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub String);

impl ConnectionId {
  /// A fresh id (a random UUID).
  pub fn fresh() -> Self {
    Self(uuid::Uuid::new_v4().to_string())
  }
}

/// The `_tag` of the failure a frame transport answers for a call refused before it ran: an
/// unknown procedure (`status` 404) or a guard's refusal (the guard's status, typically 401 or
/// 403). Members: `status`, `title`, and `tag` (the procedure tag, when the refusal names it).
pub const REJECTED_TAG: &str = "Rejected";

/// The outcome a frame transport answers for `rejection` (see [`REJECTED_TAG`]).
pub fn rejection_outcome(rejection: &Rejection) -> CallOutcome {
  let (status, tag) = match rejection {
    Rejection::UnknownProcedure { tag, .. } => (StatusCode::NOT_FOUND, Some(tag.as_str())),
    Rejection::Refused(response) => (response.status(), None),
    Rejection::WrongKind { tag, .. } => (StatusCode::BAD_REQUEST, Some(tag.as_str())),
  };
  let mut error = json!({
    "_tag": REJECTED_TAG,
    "status": status.as_u16(),
    "title": status.canonical_reason().unwrap_or(REJECTED_TAG),
  });
  if let Some(tag) = tag {
    error["tag"] = Value::String(tag.to_owned());
  }
  CallOutcome::typed(error, status)
}

/// A copy of a request head: method, URI, version, headers and extensions (the guards of every
/// call on a connection see the head of the request that opened it).
#[cfg(feature = "ws")]
pub(crate) fn copy_head(parts: &axum::http::request::Parts) -> axum::http::request::Parts {
  let mut head = axum::http::Request::new(()).into_parts().0;
  head.method = parts.method.clone();
  head.uri = parts.uri.clone();
  head.version = parts.version;
  head.headers = parts.headers.clone();
  head.extensions = parts.extensions.clone();
  head
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::response::IntoResponse;

  fn error_of(outcome: &CallOutcome) -> Value {
    serde_json::to_value(&outcome.exit).unwrap()["error"].clone()
  }

  #[test]
  fn rejections_become_rejected_failures() {
    let unknown = rejection_outcome(&Rejection::UnknownProcedure {
      tag: "A.nope".into(),
      version: Some(2),
    });
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(
      error_of(&unknown),
      json!({"_tag": "Rejected", "status": 404, "title": "Not Found", "tag": "A.nope"})
    );
    let refused = rejection_outcome(&Rejection::Refused(Box::new(
      StatusCode::FORBIDDEN.into_response(),
    )));
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.reserved().is_none());
    let wrong = rejection_outcome(&Rejection::WrongKind {
      tag: "A.b".into(),
      kind: crate::contract::ProcedureKind::Bidi,
    });
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    let odd = rejection_outcome(&Rejection::Refused(Box::new(
      StatusCode::from_u16(599).unwrap().into_response(),
    )));
    assert_eq!(error_of(&odd)["title"], "Rejected");
    assert_ne!(ConnectionId::fresh(), ConnectionId::fresh());
  }

  #[cfg(feature = "ws")]
  #[test]
  fn heads_copy_everything_a_guard_reads() {
    let mut parts = axum::http::Request::post("/rpc/ws?x=1")
      .header("authorization", "Bearer t")
      .body(())
      .unwrap()
      .into_parts()
      .0;
    parts.extensions.insert(crate::RpcPrincipal("ada".into()));
    let head = copy_head(&parts);
    assert_eq!(head.method, axum::http::Method::POST);
    assert_eq!(head.uri, "/rpc/ws?x=1");
    assert_eq!(head.headers["authorization"], "Bearer t");
    assert_eq!(
      head.extensions.get::<crate::RpcPrincipal>(),
      Some(&crate::RpcPrincipal("ada".into()))
    );
  }
}
