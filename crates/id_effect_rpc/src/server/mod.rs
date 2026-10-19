//! The RPC server: an [`RpcGroup`] served at `POST /rpc` as a **declared** typed route of
//! [`id_effect_axum::route::Router`] (finding H8: the deny-by-default router admits it), with the
//! access decision of every procedure enforced by [`dispatch`] before its handler runs.
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
//! assert_eq!(router.access_report().public, ["POST /rpc"]);
//! let app: axum::Router = router.with_state(DynEnv::new()).into_axum();
//! # let _ = app;
//! ```

pub mod dispatch;
mod http;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::Method;
use axum::routing::post;
use id_effect::DynEnv;
use id_effect_axum::route::{Endpoint, Public, RouteInfo, Router};

use crate::protocol::RPC_DISPATCH_PATH;
use crate::registry::RpcGroup;

/// Default cap on a request body (1 MiB), checked before the body is buffered.
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;

/// An [`RpcGroup`] ready to serve over HTTP.
#[derive(Clone, Debug)]
pub struct RpcServer {
  group: Arc<RpcGroup>,
  path: Arc<str>,
  max_body_bytes: usize,
}

impl RpcServer {
  /// Serve `group` at [`RPC_DISPATCH_PATH`] with a [`DEFAULT_MAX_BODY_BYTES`] body cap.
  #[inline]
  pub fn new(group: RpcGroup) -> Self {
    Self {
      group: Arc::new(group),
      path: Arc::from(RPC_DISPATCH_PATH),
      max_body_bytes: DEFAULT_MAX_BODY_BYTES,
    }
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

  /// The `POST {path}` dispatch route as a typed endpoint. It is [`Public`] at the route level —
  /// the deny-by-default router serves it — and each procedure's own decision (public or its
  /// guards) is enforced per call.
  pub fn endpoint(self) -> Endpoint<DynEnv, Public> {
    let mut info = RouteInfo::new(Method::POST, self.path.to_string());
    info.handler = Some("id_effect_rpc::server::RpcServer");
    let server = self;
    let method_router = post(move |State(env): State<DynEnv>, request: Request| {
      http::handle(server.clone(), env, request)
    });
    Endpoint::from_parts(info, method_router).public()
  }

  /// Add the dispatch route to `router`.
  pub fn mount(self, router: Router<DynEnv>) -> Router<DynEnv> {
    router.route(self.endpoint())
  }

  /// A typed router with only the dispatch route.
  pub fn router(self) -> Router<DynEnv> {
    self.mount(Router::new())
  }

  /// The plain Axum router of [`RpcServer::router`] (deny-by-default layer installed).
  #[inline]
  pub fn mount_default(self) -> axum::Router<DynEnv> {
    self.router().into_axum()
  }
}

/// Add `server`'s dispatch route to `router` (as [`RpcServer::mount`]).
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
    let mut req = Request::builder().method("POST").uri(path);
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
    let body = serde_json::json!({"tag": tag, "payload": {"name": "Ada", "enthusiasm": 1}});
    Body::from(serde_json::to_vec(&body).unwrap())
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn post_rpc_dispatches_tagged_request() {
    let app = greet_server().mount_default().with_state(DynEnv::new());
    let (status, body) = post(
      app,
      "/rpc",
      Some("application/json"),
      envelope("Greeter.greet"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"]["message"], "Hello, Ada!");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn edge_rejections_are_problems() {
    let server = greet_server().with_path("/api/rpc").with_max_body_bytes(64);
    assert_eq!(server.path(), "/api/rpc");
    assert_eq!(server.max_body_bytes(), 64);
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

    let big = Body::from(vec![b' '; 65]);
    let (status, body) = post(app.clone(), "/api/rpc", None, big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["instance"], "/api/rpc");

    let (status, body) = post(app.clone(), "/api/rpc", None, Body::from("{nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
      body["detail"]
        .as_str()
        .unwrap()
        .starts_with("malformed rpc envelope")
    );

    let broken = Body::from_stream(futures_util::stream::iter([Err::<axum::body::Bytes, _>(
      std::io::Error::other("reset"),
    )]));
    let (status, body) = post(app.clone(), "/api/rpc", None, broken).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["detail"], "the request body could not be read");

    let bad_payload = Body::from(r#"{"tag":"Greeter.greet","payload":{"name":1}}"#);
    let (status, body) = post(app, "/api/rpc", None, bad_payload).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_argument");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn failure_envelopes_map_to_statuses_with_the_correlation_id() {
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
      let (status, body) = post(app.clone(), "/rpc", None, body).await;
      assert_eq!(status, expected, "{tag}");
      assert!(body["failure"]["correlation_id"].is_string());
    }
  }
}
