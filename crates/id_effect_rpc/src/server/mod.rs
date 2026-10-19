//! The RPC server: an [`RpcGroup`] served under `/rpc` as **declared** typed routes of
//! [`id_effect_axum::route::Router`] (finding H8: the deny-by-default router admits them), with
//! the access decision of every procedure enforced by [`dispatch`] before its handler runs.
//!
//! Three routes (see [`http`](self) for what each kind answers on each):
//!
//! - `POST /rpc` — one `WireCall` envelope (query or mutation);
//! - `POST /rpc/{tag}` — the input as the body: queries, mutations, server streams (NDJSON
//!   response), client streams (NDJSON request);
//! - `GET /rpc/{tag}?input=<json>` — queries, with an ETag and `If-None-Match` → 304; with
//!   `Accept: text/event-stream`, subscriptions and server streams as server-sent events
//!   (feature `sse`, [`crate::transport`]).
//!
//! `RpcServer::with_websocket` (feature `ws`) adds a fourth, `GET /rpc/ws`: every kind
//! multiplexed on one WebSocket.
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect::{DynEnv, schema, succeed};
//! use id_effect_axum::route::Router;
//! use id_effect_rpc::{Procedure, RpcGroup, RpcServer};
//!
//! let mut group = RpcGroup::new();
//! group.register(
//!   Procedure::query("Greeter.greet").public(),
//!   Arc::new(schema::string::<()>()),
//!   Arc::new(schema::string::<()>()),
//!   &["message"],
//!   |name, _env| succeed(format!("Hello, {name}!")),
//! );
//! let router: Router<DynEnv> = RpcServer::new(group).mount(Router::new());
//! let mut public = router.access_report().public.clone();
//! public.sort();
//! assert_eq!(public, ["GET /rpc/{tag}", "POST /rpc", "POST /rpc/{tag}"]);
//! let app: axum::Router = router.with_state(DynEnv::new()).into_axum();
//! # let _ = app;
//! ```
//!
//! Defaults, all finite: request bodies ≤ [`DEFAULT_MAX_BODY_BYTES`] (1 MiB), every call's
//! deadline ≤ [`DEFAULT_MAX_DEADLINE`] (30 s, the client's `x-rpc-deadline-ms` is clamped to it),
//! at most [`DEFAULT_STREAM_WINDOW`] unread stream items per call, client streams within their
//! procedure's item and byte caps, and mutation idempotency in a bounded
//! [`MemoryIdempotencyStore`] (24 h, 100,000 keys).

pub mod dispatch;
mod http;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Request, State};
use axum::http::Method;
use axum::http::request::Parts;
use axum::routing::{get, post};
use id_effect::DynEnv;
use id_effect_axum::route::{Endpoint, Public, RouteInfo, Router};

pub use crate::cancel::DEFAULT_MAX_DEADLINE;
use crate::idempotency::{IdempotencyStore, MemoryIdempotencyStore};
use crate::protocol::RPC_DISPATCH_PATH;
use crate::registry::RpcGroup;
use http::Route;

/// Default cap on a request body (1 MiB), checked before the body is buffered.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Default number of stream items a call may have sent that the transport has not written yet.
pub const DEFAULT_STREAM_WINDOW: usize = 32;

/// Header every first-party client sends on calls (`x-rpc: 1`, ADR 0014 C5); a `POST` without it
/// is refused.
pub const RPC_HEADER: &str = "x-rpc";

/// Header naming the service version on `GET/POST /rpc/{tag}` (the envelope uses `v`).
pub const VERSION_HEADER: &str = "x-rpc-version";

/// The authenticated principal of a request, as a request extension an authentication layer
/// inserts; the default principal resolver reads it ([`RpcServer::with_principal`]). Idempotency
/// keys are scoped by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcPrincipal(pub String);

type PrincipalFn = Arc<dyn Fn(&Parts) -> Option<String> + Send + Sync>;

/// An [`RpcGroup`] ready to serve over HTTP.
#[derive(Clone)]
pub struct RpcServer {
  group: Arc<RpcGroup>,
  path: Arc<str>,
  max_body_bytes: usize,
  max_deadline: Duration,
  stream_window: usize,
  idempotency: Arc<dyn IdempotencyStore>,
  principal: PrincipalFn,
  /// Batch limits and parallelism (`crate::batch`, r12-e12-1-batching).
  pub(crate) batch: crate::batch::BatchConfig,
  /// `GET {path}/ws` when set ([`RpcServer::with_websocket`]).
  #[cfg(feature = "ws")]
  websocket: Option<crate::transport::ws::RpcWsConfig>,
  /// Subscriptions and server streams as server-sent events on `GET {path}/{tag}`.
  #[cfg(feature = "sse")]
  sse: crate::transport::sse::SseMount,
}

impl fmt::Debug for RpcServer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RpcServer")
      .field("path", &self.path)
      .field("procedures", &self.group.len())
      .field("max_body_bytes", &self.max_body_bytes)
      .field("max_deadline", &self.max_deadline)
      .field("stream_window", &self.stream_window)
      .finish_non_exhaustive()
  }
}

impl RpcServer {
  /// Serve `group` at [`RPC_DISPATCH_PATH`] with the defaults above.
  pub fn new(group: RpcGroup) -> Self {
    Self {
      group: Arc::new(group),
      path: Arc::from(RPC_DISPATCH_PATH),
      max_body_bytes: DEFAULT_MAX_BODY_BYTES,
      max_deadline: DEFAULT_MAX_DEADLINE,
      stream_window: DEFAULT_STREAM_WINDOW,
      idempotency: Arc::new(MemoryIdempotencyStore::new()),
      principal: Arc::new(|parts: &Parts| {
        parts
          .extensions
          .get::<RpcPrincipal>()
          .map(|RpcPrincipal(p)| p.clone())
      }),
      batch: crate::batch::BatchConfig::default(),
      #[cfg(feature = "ws")]
      websocket: None,
      #[cfg(feature = "sse")]
      sse: crate::transport::sse::SseMount::new(id_effect_axum::realtime::sse::SseConfig::default()),
    }
  }

  /// Also serve every procedure over WebSocket at `GET {path}/ws` (feature `ws`; see
  /// [`crate::transport::ws`]): `config` holds the socket limits, the `Origin` allowlist (empty by
  /// default: browser origins are refused until listed) and the per-connection call cap.
  #[cfg(feature = "ws")]
  #[must_use]
  pub fn with_websocket(mut self, config: crate::transport::ws::RpcWsConfig) -> Self {
    self.websocket = Some(config);
    self
  }

  /// The stream settings and connection caps of the server-sent events that serve
  /// subscriptions and server streams on `GET {path}/{tag}` with `Accept: text/event-stream`
  /// (feature `sse`, on by default with it: [`SseConfig::default`](id_effect_axum::realtime::sse::SseConfig)).
  #[cfg(feature = "sse")]
  #[must_use]
  pub fn with_sse(mut self, config: id_effect_axum::realtime::sse::SseConfig) -> Self {
    self.sse = crate::transport::sse::SseMount::new(config);
    self
  }

  #[cfg(feature = "sse")]
  pub(crate) fn sse_mount(&self) -> &crate::transport::sse::SseMount {
    &self.sse
  }

  /// Serve at `path` instead of `/rpc`.
  #[must_use]
  pub fn with_path(mut self, path: impl Into<String>) -> Self {
    self.path = Arc::from(path.into());
    self
  }

  /// Cap request bodies at `bytes` (over → 413 Problem).
  #[must_use]
  pub fn with_max_body_bytes(mut self, bytes: usize) -> Self {
    self.max_body_bytes = bytes;
    self
  }

  /// Cap every call's deadline at `max` (expired → `DeadlineExceeded`, the handler interrupted).
  #[must_use]
  pub fn with_max_deadline(mut self, max: Duration) -> Self {
    self.max_deadline = max;
    self
  }

  /// Let a stream call get at most `items` (≥ 1) items ahead of the transport.
  #[must_use]
  pub fn with_stream_window(mut self, items: usize) -> Self {
    self.stream_window = items.max(1);
    self
  }

  /// Keep mutation results for `Idempotency-Key` replays in `store`.
  #[must_use]
  pub fn with_idempotency_store(mut self, store: impl IdempotencyStore) -> Self {
    self.idempotency = Arc::new(store);
    self
  }

  /// Resolve a request's authenticated principal with `resolve` (default: the
  /// [`RpcPrincipal`] request extension); `None` is an anonymous caller.
  #[must_use]
  pub fn with_principal(
    mut self,
    resolve: impl Fn(&Parts) -> Option<String> + Send + Sync + 'static,
  ) -> Self {
    self.principal = Arc::new(resolve);
    self
  }

  /// Borrow the underlying group.
  #[inline]
  pub fn group(&self) -> &RpcGroup {
    &self.group
  }

  /// The dispatch path.
  pub fn path(&self) -> &str {
    &self.path
  }

  /// The request body cap.
  pub fn max_body_bytes(&self) -> usize {
    self.max_body_bytes
  }

  /// The deadline cap.
  pub fn max_deadline(&self) -> Duration {
    self.max_deadline
  }

  /// The stream window.
  pub fn stream_window(&self) -> usize {
    self.stream_window
  }

  /// The idempotency store.
  pub fn idempotency_store(&self) -> Arc<dyn IdempotencyStore> {
    self.idempotency.clone()
  }

  /// The principal of a request head.
  pub fn principal_of(&self, parts: &Parts) -> Option<String> {
    (self.principal)(parts)
  }

  fn tag_path(&self) -> String {
    format!("{}/{{tag}}", self.path.trim_end_matches('/'))
  }

  /// The three routes as typed endpoints. Each is [`Public`] at the route level — the
  /// deny-by-default router serves it — and each procedure's own decision (public or its
  /// guards) is enforced per call.
  pub fn endpoints(self) -> Vec<Endpoint<DynEnv, Public>> {
    // Shared by the routes: a request takes a reference count, not a copy of the server.
    let shared = Arc::new(self.clone());
    let envelope = {
      let server = shared.clone();
      let mut info = RouteInfo::new(Method::POST, self.path.to_string());
      info.handler = Some("id_effect_rpc::server::RpcServer");
      let method_router = post(move |State(env): State<DynEnv>, request: Request| {
        http::handle(server.clone(), env, Route::Envelope, request)
      });
      Endpoint::from_parts(info, method_router).public()
    };
    let by_tag = |method: Method| {
      let server = shared.clone();
      let mut info = RouteInfo::new(method.clone(), self.tag_path());
      info.handler = Some("id_effect_rpc::server::RpcServer");
      let handler = move |State(env): State<DynEnv>, Path(tag): Path<String>, request: Request| {
        http::handle(server.clone(), env, Route::Tag(tag), request)
      };
      let method_router = if method == Method::GET {
        get(handler)
      } else {
        post(handler)
      };
      Endpoint::from_parts(info, method_router).public()
    };
    #[allow(unused_mut)]
    let mut endpoints = vec![envelope, by_tag(Method::POST), by_tag(Method::GET)];
    #[cfg(feature = "ws")]
    if let Some(config) = self.websocket.clone() {
      endpoints.push(crate::transport::ws::endpoint(self, config));
    }
    endpoints
  }

  /// Add the routes to `router`.
  pub fn mount(self, router: Router<DynEnv>) -> Router<DynEnv> {
    self
      .endpoints()
      .into_iter()
      .fold(router, |router, endpoint| router.route(endpoint))
  }

  /// A typed router with only the RPC routes.
  pub fn router(self) -> Router<DynEnv> {
    self.mount(Router::new())
  }

  /// The plain Axum router of [`RpcServer::router`] (deny-by-default layer installed).
  #[inline]
  pub fn mount_default(self) -> axum::Router<DynEnv> {
    self.router().into_axum()
  }
}

/// Add `server`'s routes to `router` (as [`RpcServer::mount`]).
pub fn layer_rpc(router: Router<DynEnv>, server: RpcServer) -> Router<DynEnv> {
  server.mount(router)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::registry::Procedure;
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use http_body_util::BodyExt;
  use id_effect::{schema, succeed};
  use tower::ServiceExt;

  fn greet_server() -> RpcServer {
    let mut group = RpcGroup::new();
    group.register(
      Procedure::query("Greeter.greet").public(),
      Arc::new(schema::struct_(
        "name",
        schema::string::<()>(),
        "enthusiasm",
        schema::i64::<()>(),
      )),
      Arc::new(schema::struct_(
        "message",
        schema::string::<()>(),
        "count",
        schema::i64::<()>(),
      )),
      &["message", "count"],
      |(name, n), _| succeed((format!("Hello, {name}!"), n)),
    );
    RpcServer::new(group)
  }

  async fn post(
    app: axum::Router,
    path: &str,
    content_type: Option<&str>,
    body: Body,
  ) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
      .method("POST")
      .uri(path)
      .header("x-rpc", "1");
    if let Some(ct) = content_type {
      req = req.header("content-type", ct);
    }
    let res = app.oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    assert!(res.headers().contains_key("x-correlation-id"));
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
      status,
      serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
  }

  fn envelope(tag: &str) -> Body {
    let body =
      serde_json::json!({"id": 4, "tag": tag, "payload": {"name": "Ada", "enthusiasm": 1}});
    Body::from(serde_json::to_vec(&body).unwrap())
  }

  const JSON: Option<&str> = Some("application/json");

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn post_rpc_dispatches_a_wire_call() {
    let app = greet_server().mount_default().with_state(DynEnv::new());
    let (status, body) = post(app, "/rpc", JSON, envelope("Greeter.greet")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
      body,
      serde_json::json!({"id": 4, "tag": "Greeter.greet", "exit": {"_tag": "Success", "value": {"message": "Hello, Ada!", "count": 1}}})
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn edge_rejections_are_problems() {
    let server = greet_server()
      .with_path("/api/rpc")
      .with_max_body_bytes(64)
      .with_max_deadline(Duration::from_secs(2))
      .with_stream_window(0)
      .with_idempotency_store(MemoryIdempotencyStore::new())
      .with_principal(|_| None);
    assert_eq!(server.path(), "/api/rpc");
    assert_eq!(server.max_body_bytes(), 64);
    assert_eq!(server.max_deadline(), Duration::from_secs(2));
    assert_eq!(server.stream_window(), 1);
    assert_eq!(server.group().len(), 1);
    assert!(format!("{server:?}").contains("/api/rpc"));
    let app = layer_rpc(Router::new(), server)
      .with_state(DynEnv::new())
      .into_axum();

    let (status, body) = post(
      app.clone(),
      "/api/rpc",
      Some("text/plain"),
      envelope("Greeter.greet"),
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["status"], 415);

    let (status, _) = post(app.clone(), "/api/rpc", None, envelope("Greeter.greet")).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let big = Body::from(vec![b' '; 65]);
    let (status, body) = post(app.clone(), "/api/rpc", JSON, big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["instance"], "/api/rpc");

    let (status, body) = post(app.clone(), "/api/rpc", JSON, Body::from("{nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
      body["detail"]
        .as_str()
        .unwrap()
        .starts_with("malformed rpc envelope")
    );

    let (status, body) = post(app.clone(), "/api/rpc", JSON, Body::from(" [ ]")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["detail"].as_str().unwrap().contains("batches"));

    let broken = Body::from_stream(futures_util::stream::iter([Err::<axum::body::Bytes, _>(
      std::io::Error::other("reset"),
    )]));
    let (status, body) = post(app.clone(), "/api/rpc", JSON, broken).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "the request body could not be read");

    let bad_payload = Body::from(r#"{"tag":"Greeter.greet","payload":{"name":1}}"#);
    let (status, body) = post(app, "/api/rpc", JSON, bad_payload).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["exit"]["error"]["_tag"], "InvalidInput");
    assert_eq!(body["exit"]["error"]["issues"][0]["path"], "/name");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn typed_failures_map_to_their_statuses() {
    type Case = (&'static str, fn() -> crate::RpcError, StatusCode);
    let mut group = RpcGroup::new();
    let errors: [Case; 5] = [
      (
        "A.bad",
        || crate::RpcError::invalid_argument("x"),
        StatusCode::BAD_REQUEST,
      ),
      (
        "A.missing",
        || crate::RpcError::not_found("x"),
        StatusCode::NOT_FOUND,
      ),
      (
        "A.dup",
        || crate::RpcError::already_exists("x"),
        StatusCode::CONFLICT,
      ),
      (
        "A.anon",
        || crate::RpcError::unauthenticated("x"),
        StatusCode::UNAUTHORIZED,
      ),
      (
        "A.boom",
        || crate::RpcError::internal("x"),
        StatusCode::INTERNAL_SERVER_ERROR,
      ),
    ];
    for (tag, make, _) in errors {
      group.register(
        Procedure::mutation(tag).public(),
        Arc::new(schema::string::<()>()),
        Arc::new(schema::string::<()>()),
        &["v"],
        move |_, _| id_effect::fail(make()),
      );
    }
    let app = RpcServer::new(group)
      .mount_default()
      .with_state(DynEnv::new());
    for (tag, _, expected) in errors {
      let body = Body::from(format!(r#"{{"tag":"{tag}","payload":"x"}}"#));
      let (status, body) = post(app.clone(), "/rpc", JSON, body).await;
      assert_eq!(status, expected, "{tag}");
      assert_eq!(body["exit"]["_tag"], "Failure");
    }
  }
}
