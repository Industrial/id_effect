//! RPC HTTP server — mount tagged dispatch on Axum (`RpcServer.layer` parity).

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use id_effect::Env;

use crate::RpcError;
use crate::correlation::{append_correlation_header, ensure_correlation_id};
use crate::protocol::{RPC_DISPATCH_PATH, RpcWireResponse};
use crate::registry::RpcGroup;
use crate::serialization::{decode_wire_request, encode_wire_response, serialization_to_rpc_error};
use crate::span::{record_request_metadata, rpc_request_span};

/// Shared RPC server state: method group + capability environment factory.
#[derive(Clone)]
pub struct RpcServer {
  group: Arc<RpcGroup>,
}

impl RpcServer {
  /// Wrap an [`RpcGroup`] for HTTP serving.
  #[inline]
  pub fn new(group: RpcGroup) -> Self {
    Self {
      group: Arc::new(group),
    }
  }

  /// Borrow the underlying group.
  #[inline]
  pub fn group(&self) -> &RpcGroup {
    &self.group
  }

  /// Mount `POST {path}` tagged dispatch handler on a router with [`Env`] state.
  pub fn mount_dispatch(self, onto: Router<Env>, path: &str) -> Router<Env> {
    let group = self.group.clone();
    onto.route(
      path,
      post(
        move |State(env): State<Env>, headers: HeaderMap, body: Bytes| async move {
          dispatch_request(env, group.clone(), &headers, &body).await
        },
      ),
    )
  }

  /// Mount default `POST /rpc` dispatch route on a new router.
  #[inline]
  pub fn mount_default(self) -> Router<Env> {
    Self::mount_dispatch(self, Router::new(), RPC_DISPATCH_PATH)
  }
}

async fn dispatch_request(
  env: Env,
  group: Arc<RpcGroup>,
  headers: &HeaderMap,
  body: &[u8],
) -> Response {
  let cid = ensure_correlation_id(headers);
  let span = rpc_request_span();
  let _enter = span.enter();
  record_request_metadata(
    &span,
    Some(cid.as_str()),
    "POST",
    RPC_DISPATCH_PATH,
    "rpc.dispatch",
  );

  let req = match decode_wire_request(body) {
    Ok(r) => r,
    Err(e) => {
      let err = serialization_to_rpc_error(e).with_correlation_id(cid.clone());
      return err.into_response();
    }
  };

  record_request_metadata(
    &span,
    Some(cid.as_str()),
    "POST",
    RPC_DISPATCH_PATH,
    req.tag.as_str(),
  );

  let Some(method) = group.get(&req.tag) else {
    let err = RpcError::not_found(format!("unknown rpc tag: {}", req.tag)).with_correlation_id(cid);
    return err.into_response();
  };

  match method.dispatch(req.payload, env).await {
    Ok(wire) => wire_to_response(wire, &cid),
    Err(e) => serialization_to_rpc_error(e)
      .with_correlation_id(cid)
      .into_response(),
  }
}

fn wire_to_response(wire: RpcWireResponse, cid: &str) -> Response {
  let status = match &wire {
    RpcWireResponse::Success { .. } => StatusCode::OK,
    RpcWireResponse::Failure { failure, .. } => match failure.code {
      crate::envelope::RpcErrorCode::InvalidArgument => StatusCode::BAD_REQUEST,
      crate::envelope::RpcErrorCode::NotFound => StatusCode::NOT_FOUND,
      crate::envelope::RpcErrorCode::AlreadyExists => StatusCode::CONFLICT,
      crate::envelope::RpcErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
      crate::envelope::RpcErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    },
  };
  let mut wire = wire;
  if let RpcWireResponse::Failure { failure, .. } = &mut wire
    && failure.correlation_id.is_none()
  {
    failure.correlation_id = Some(cid.to_owned());
  }
  let bytes = encode_wire_response(&wire).unwrap_or_default();
  let mut res = (status, bytes).into_response();
  append_correlation_header(res.headers_mut(), cid);
  res
}

/// Layer helper: add RPC dispatch routes to an existing Axum router.
pub fn layer_rpc(router: Router<Env>, server: RpcServer) -> Router<Env> {
  server.mount_dispatch(router, RPC_DISPATCH_PATH)
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::Body;
  use axum::http::Request;
  use id_effect::{schema, succeed};
  use std::sync::Arc;
  use tower::ServiceExt;

  fn greet_server() -> RpcServer {
    let payload = Arc::new(schema::struct_(
      "name",
      schema::string::<()>(),
      "enthusiasm",
      schema::i64::<()>(),
    ));
    let success = Arc::new(schema::struct_(
      "message",
      schema::string::<()>(),
      "count",
      schema::i64::<()>(),
    ));
    let mut group = RpcGroup::new();
    group.register(
      "greet",
      payload,
      &["name", "enthusiasm"],
      success,
      &["message", "count"],
      |(name, n), _| succeed((format!("Hello, {name}!"), n)),
    );
    RpcServer::new(group)
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn post_rpc_dispatches_tagged_request() {
    let app = greet_server().mount_default().with_state(Env::new());
    let body = serde_json::json!({
      "tag": "greet",
      "payload": {"name": "Ada", "enthusiasm": 1}
    });
    let req = Request::builder()
      .method("POST")
      .uri("/rpc")
      .header("content-type", "application/json")
      .body(Body::from(serde_json::to_vec(&body).unwrap()))
      .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }
}
