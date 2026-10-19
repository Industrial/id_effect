//! The HTTP edge of [`super::RpcServer`]: one `POST` handler that caps and decodes the body,
//! dispatches the call ([`super::dispatch`]) and maps the outcome to a response.
//!
//! Transport-level rejections are RFC 9457 Problem Details (`application/problem+json`): an
//! unknown codec is **415**, a body over the cap **413**, a malformed envelope **400**, an
//! unknown tag **404**; a guard's refusal is the guard's own response. A procedure's outcome is
//! the tagged wire response of [`crate::protocol`].

use std::error::Error as _;

use axum::body::Body;
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::DynEnv;
use id_effect_axum::route::Problem;
use tracing::Instrument as _;

use crate::codec::negotiate_content_type;
use crate::correlation::{append_correlation_header, ensure_correlation_id};
use crate::envelope::RpcErrorCode;
use crate::protocol::RpcWireResponse;
use crate::serialization::{decode_wire_request, encode_wire_response, serialization_to_rpc_error};
use crate::span::{record_request_metadata, rpc_request_span};

use super::RpcServer;
use super::dispatch::{Rejection, dispatch};

/// Serve one `POST {path}` request.
pub(crate) async fn handle(server: RpcServer, env: DynEnv, request: Request) -> Response {
  let cid = ensure_correlation_id(request.headers());
  let span = rpc_request_span();
  record_request_metadata(
    &span,
    Some(cid.as_str()),
    "POST",
    server.path(),
    "rpc.dispatch",
  );
  let (parts, body) = request.into_parts();
  let mut response = respond(&server, env, parts, body, &span, &cid)
    .instrument(span.clone())
    .await;
  append_correlation_header(response.headers_mut(), &cid);
  response
}

async fn respond(
  server: &RpcServer,
  env: DynEnv,
  parts: Parts,
  body: Body,
  span: &tracing::Span,
  cid: &str,
) -> Response {
  let path = server.path();
  let content_type = parts
    .headers
    .get(header::CONTENT_TYPE)
    .map(|v| v.to_str().unwrap_or("(not visible ASCII)"));
  if let Err(err) = negotiate_content_type(content_type) {
    return problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, err.to_string(), path).into_response();
  }
  let limit = server.max_body_bytes();
  let bytes = match axum::body::to_bytes(body, limit).await {
    Ok(bytes) => bytes,
    Err(err) if is_length_limit(&err) => {
      let detail = format!("the request body exceeds {limit} bytes");
      return problem(StatusCode::PAYLOAD_TOO_LARGE, detail, path).into_response();
    }
    Err(_) => {
      let detail = "the request body could not be read";
      return problem(StatusCode::BAD_REQUEST, detail, path).into_response();
    }
  };
  let call = match decode_wire_request(&bytes) {
    Ok(call) => call,
    Err(err) => {
      let detail = format!("malformed rpc envelope: {err}");
      return problem(StatusCode::BAD_REQUEST, detail, path).into_response();
    }
  };
  record_request_metadata(span, Some(cid), "POST", path, call.tag.as_str());
  match dispatch(server.group(), &parts, call, env).await {
    Ok(wire) => wire_to_response(wire, cid),
    Err(Rejection::UnknownProcedure { tag }) => {
      problem(StatusCode::NOT_FOUND, "unknown rpc procedure", path)
        .with_extension("tag", serde_json::Value::String(tag))
        .into_response()
    }
    Err(Rejection::Refused(response)) => *response,
    Err(Rejection::InvalidPayload(err)) => serialization_to_rpc_error(err)
      .with_correlation_id(cid)
      .into_response(),
  }
}

/// Whether `err` (from [`axum::body::to_bytes`]) is the length limit.
fn is_length_limit(err: &axum::Error) -> bool {
  let mut source = err.source();
  while let Some(cause) = source {
    if cause.is::<http_body_util::LengthLimitError>() {
      return true;
    }
    source = cause.source();
  }
  false
}

fn problem(status: StatusCode, detail: impl Into<String>, path: &str) -> Problem {
  Problem::new(status).with_detail(detail).with_instance(path)
}

fn wire_to_response(mut wire: RpcWireResponse, cid: &str) -> Response {
  let status = match &wire {
    RpcWireResponse::Success { .. } => StatusCode::OK,
    RpcWireResponse::Failure { failure, .. } => match failure.code {
      RpcErrorCode::InvalidArgument => StatusCode::BAD_REQUEST,
      RpcErrorCode::NotFound => StatusCode::NOT_FOUND,
      RpcErrorCode::AlreadyExists => StatusCode::CONFLICT,
      RpcErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
      RpcErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    },
  };
  if let RpcWireResponse::Failure { failure, .. } = &mut wire
    && failure.correlation_id.is_none()
  {
    failure.correlation_id = Some(cid.to_owned());
  }
  // Strings and `serde_json::Value`s always serialize.
  let bytes = encode_wire_response(&wire).unwrap_or_default();
  (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}
