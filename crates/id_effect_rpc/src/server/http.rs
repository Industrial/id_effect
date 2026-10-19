//! The HTTP edge of [`super::RpcServer`] (design note §2): the envelope route `POST {path}`
//! and the per-procedure routes `POST {path}/{tag}` and `GET {path}/{tag}`.
//!
//! | Kind | `POST {path}` (envelope) | `POST {path}/{tag}` | `GET {path}/{tag}` |
//! |---|---|---|---|
//! | query | `WireCall` → `WireResult` | input body → `WireResult` | `?input=<json>`, ETag / 304 |
//! | mutation | `WireCall` → `WireResult`, `Idempotency-Key` | input body, `Idempotency-Key` | 405 |
//! | server stream | 400 | input body → NDJSON frames | 405 |
//! | client stream | 400 | NDJSON items (item and byte caps) → `WireResult` | 405 |
//! | bidi | 426 | 426 | 426 |
//! | subscription | 400 | 406 (SSE and WebSocket transports) | 406 |
//!
//! Every `POST` needs a codec `Content-Type` (`application/json`; `application/x-ndjson` for a
//! client stream) and the header `x-rpc: 1`, so a browser form or a simple cross-site request
//! cannot call a procedure (C5, ADR 0013 R9): another content type is **415**, a missing header
//! **403**. Transport-level rejections are RFC 9457 Problem Details: unknown tag **404**,
//! malformed envelope **400**, unknown codec **415**, body over the cap **413**, a guard's
//! refusal the guard's own response. Once a procedure ran (or its input failed decoding) the
//! answer is a [`WireResult`] with the status of its exit.

// The early-return `Err(Response)` of the edge helpers is the answer itself, built once per
// request; boxing it would only add an allocation.
#![allow(clippy::result_large_err)]

use std::convert::Infallible;
use std::error::Error as _;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use futures_util::future::{Either, select};
use id_effect::DynEnv;
use id_effect_axum::route::Problem;
use id_effect_axum::{ClientIp, TrustedProxies};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tracing::Instrument as _;

use crate::cancel::{CallControl, effective_deadline, requested_deadline};
use crate::codec::{CodecId, NDJSON, negotiate_content_type};
use crate::contract::ProcedureKind;
use crate::correlation::{append_correlation_header, ensure_correlation_id};
use crate::handler::{CallContext, CallOutcome, InboundSender};
use crate::idempotency::{
  Begin, IDEMPOTENCY_KEY_HEADER, IdempotencyKey, IdempotencyScope, REPLAYED_HEADER, Reservation,
  StoredResult, parse_key, payload_digest,
};
use crate::registry::RpcMethodEntry;
use crate::span::{record_request_metadata, rpc_request_span};
use crate::wire::{
  CallId, InputIssue, MAX_FRAME_BYTES, RpcFailure, WireCall, WireExit, WireResult,
};

use super::dispatch::{self, Rejection};
use super::{RPC_HEADER, RpcServer, VERSION_HEADER};

/// The call id of the frames of a single-call NDJSON stream.
const NDJSON_CALL_ID: u64 = 1;

/// Which route a request came in on.
pub(crate) enum Route {
  /// `POST {path}`.
  Envelope,
  /// `POST {path}/{tag}` or `GET {path}/{tag}`.
  Tag(String),
}

/// Serve one request.
pub(crate) async fn handle(
  server: RpcServer,
  env: DynEnv,
  route: Route,
  request: Request,
) -> Response {
  let cid = ensure_correlation_id(request.headers());
  let span = rpc_request_span();
  let method = request.method().clone();
  let operation = match &route {
    Route::Envelope => "rpc.dispatch",
    Route::Tag(tag) => tag.as_str(),
  };
  record_request_metadata(
    &span,
    Some(cid.as_str()),
    method.as_str(),
    server.path(),
    operation,
  );
  let (parts, body) = request.into_parts();
  let edge = Edge {
    server: &server,
    ctx: CallContext::new(env, cid.as_str()),
    span: &span,
  };
  let mut response = match route {
    Route::Envelope => edge.envelope(parts, body).instrument(span.clone()).await,
    Route::Tag(tag) if method == Method::GET => {
      edge.get_tag(parts, tag).instrument(span.clone()).await
    }
    Route::Tag(tag) => {
      edge
        .post_tag(parts, body, tag)
        .instrument(span.clone())
        .await
    }
  };
  append_correlation_header(response.headers_mut(), &cid);
  response
}

fn problem(status: StatusCode, detail: impl Into<String>, path: &str) -> Problem {
  Problem::new(status).with_detail(detail).with_instance(path)
}

fn coded(status: StatusCode, code: &str, detail: impl Into<String>, path: &str) -> Response {
  problem(status, detail, path)
    .with_extension("code", Value::String(code.to_owned()))
    .into_response()
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

fn content_type(headers: &HeaderMap) -> Option<&str> {
  headers
    .get(header::CONTENT_TYPE)
    .map(|v| v.to_str().unwrap_or("(not visible ASCII)"))
}

fn json_response(status: StatusCode, codec: CodecId, body: Vec<u8>) -> Response {
  (status, [(header::CONTENT_TYPE, codec.content_type())], body).into_response()
}

fn encode_result(codec: CodecId, result: &WireResult) -> Vec<u8> {
  // A `WireResult` of JSON values always encodes.
  codec.encode(result).unwrap_or_default()
}

/// A strong ETag of an encoded body: the first 128 bits of its SHA-256, hex.
fn etag_of(body: &[u8]) -> String {
  let digest = Sha256::digest(body);
  let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
  format!("\"{hex}\"")
}

/// Whether `If-None-Match` names `etag` (or `*`); weak validators compare by their opaque part.
fn none_match(headers: &HeaderMap, etag: &str) -> bool {
  headers
    .get_all(header::IF_NONE_MATCH)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .map(|t| t.trim().trim_start_matches("W/"))
    .any(|t| t == "*" || t == etag)
}

/// The request's codec, NDJSON check and `x-rpc` header: the C5 rules of every `POST`.
fn post_rules(parts: &Parts, path: &str, ndjson: bool) -> Result<CodecId, Response> {
  let Some(raw) = content_type(&parts.headers) else {
    let detail = "an rpc call needs a Content-Type (application/json)";
    return Err(problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, detail, path).into_response());
  };
  let codec = if ndjson {
    let essence = raw.split(';').next().unwrap_or_default().trim();
    if !essence.eq_ignore_ascii_case(NDJSON) {
      let detail = format!("a client stream is sent as {NDJSON}, not `{essence}`");
      return Err(problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, detail, path).into_response());
    }
    CodecId::Json
  } else {
    negotiate_content_type(Some(raw)).map_err(|err| {
      problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, err.to_string(), path).into_response()
    })?
  };
  if parts.headers.get(RPC_HEADER).is_none_or(|v| v != "1") {
    return Err(coded(
      StatusCode::FORBIDDEN,
      "RpcHeaderRequired",
      "an rpc call needs the header `x-rpc: 1`",
      path,
    ));
  }
  Ok(codec)
}

fn version_of(headers: &HeaderMap, path: &str) -> Result<Option<u32>, Response> {
  let Some(raw) = headers.get(VERSION_HEADER) else {
    return Ok(None);
  };
  raw
    .to_str()
    .ok()
    .and_then(|v| v.trim().parse::<u32>().ok())
    .filter(|v| *v >= 1)
    .map(Some)
    .ok_or_else(|| {
      let detail = "x-rpc-version must be a version number (1 or more)";
      problem(StatusCode::BAD_REQUEST, detail, path).into_response()
    })
}

struct Edge<'a> {
  server: &'a RpcServer,
  ctx: CallContext,
  span: &'a tracing::Span,
}

impl Edge<'_> {
  fn path(&self) -> &str {
    self.server.path()
  }

  fn control(&self, parts: &Parts) -> CallControl {
    let deadline = effective_deadline(
      requested_deadline(&parts.headers),
      self.server.max_deadline(),
    );
    CallControl::none().with_deadline(deadline)
  }

  fn rejection(&self, rejection: Rejection) -> Response {
    let path = self.path();
    match rejection {
      Rejection::UnknownProcedure { tag, version } => {
        let mut p = problem(StatusCode::NOT_FOUND, "unknown rpc procedure", path)
          .with_extension("tag", Value::String(tag));
        if let Some(v) = version {
          p = p.with_extension("version", Value::from(v));
        }
        p.into_response()
      }
      Rejection::Refused(response) => *response,
      Rejection::WrongKind { tag, kind } => self.wrong_kind(&tag, kind, true),
    }
  }

  fn wrong_kind(&self, tag: &str, kind: ProcedureKind, envelope: bool) -> Response {
    let path = self.path();
    let (status, detail) = match kind {
      ProcedureKind::Bidi => (
        StatusCode::UPGRADE_REQUIRED,
        format!("`{tag}` is a two-way stream: open a WebSocket or WebTransport session"),
      ),
      ProcedureKind::Subscription if !envelope => (
        StatusCode::NOT_ACCEPTABLE,
        format!("`{tag}` is a subscription: subscribe over SSE or WebSocket"),
      ),
      kind if envelope => (
        StatusCode::BAD_REQUEST,
        format!("`{tag}` is a {kind}: call it at {path}/{tag}"),
      ),
      kind => (
        StatusCode::METHOD_NOT_ALLOWED,
        format!("`{tag}` is a {kind}: call it with POST"),
      ),
    };
    let mut response = problem(status, detail, path)
      .with_extension("tag", Value::String(tag.to_owned()))
      .with_extension("kind", Value::String(kind.as_str().to_owned()))
      .into_response();
    if status == StatusCode::UPGRADE_REQUIRED {
      response
        .headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    } else if status == StatusCode::METHOD_NOT_ALLOWED {
      response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    }
    response
  }

  async fn body(&self, body: Body) -> Result<Bytes, Response> {
    let limit = self.server.max_body_bytes();
    axum::body::to_bytes(body, limit).await.map_err(|err| {
      let path = self.path();
      if is_length_limit(&err) {
        let detail = format!("the request body exceeds {limit} bytes");
        problem(StatusCode::PAYLOAD_TOO_LARGE, detail, path).into_response()
      } else {
        problem(
          StatusCode::BAD_REQUEST,
          "the request body could not be read",
          path,
        )
        .into_response()
      }
    })
  }

  /// `POST {path}`: one `WireCall` envelope.
  async fn envelope(&self, parts: Parts, body: Body) -> Response {
    let path = self.path();
    let codec = match post_rules(&parts, path, false) {
      Ok(codec) => codec,
      Err(response) => return response,
    };
    let bytes = match self.body(body).await {
      Ok(bytes) => bytes,
      Err(response) => return response,
    };
    // r12-e12-1-batching: a JSON array is a batch; it answers here until that leaf serves it.
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
      let detail = "rpc batches are not served by this server";
      return problem(StatusCode::BAD_REQUEST, detail, path).into_response();
    }
    let call: WireCall = match codec.decode(&bytes) {
      Ok(call) => call,
      Err(err) => {
        let detail = format!("malformed rpc envelope: {err}");
        return problem(StatusCode::BAD_REQUEST, detail, path).into_response();
      }
    };
    record_request_metadata(
      self.span,
      Some(&self.ctx.correlation_id),
      "POST",
      path,
      &call.tag,
    );
    let entry = match dispatch::resolve(self.server.group(), &call.tag, call.v) {
      Ok(entry) => entry,
      Err(rejection) => return self.rejection(rejection),
    };
    // A stream kind is refused by `start_unary` after the guards ran (`WrongKind`).
    if let Err(rejection) = dispatch::authorize(entry, &parts).await {
      return self.rejection(rejection);
    }
    let WireCall {
      id, tag, payload, ..
    } = call;
    self.unary(entry, &parts, codec, id, tag, payload).await
  }

  /// `POST {path}/{tag}`.
  async fn post_tag(&self, parts: Parts, body: Body, tag: String) -> Response {
    let path = self.path();
    let version = match version_of(&parts.headers, path) {
      Ok(version) => version,
      Err(response) => return response,
    };
    let entry = match dispatch::resolve(self.server.group(), &tag, version) {
      Ok(entry) => entry,
      Err(rejection) => return self.rejection(rejection),
    };
    let kind = entry.kind();
    if matches!(kind, ProcedureKind::Bidi | ProcedureKind::Subscription) {
      return self.wrong_kind(&tag, kind, false);
    }
    let codec = match post_rules(&parts, path, kind == ProcedureKind::ClientStream) {
      Ok(codec) => codec,
      Err(response) => return response,
    };
    if let Err(rejection) = dispatch::authorize(entry, &parts).await {
      return self.rejection(rejection);
    }
    if kind == ProcedureKind::ClientStream {
      return self.client_stream(entry, &parts, body, codec, tag).await;
    }
    let bytes = match self.body(body).await {
      Ok(bytes) => bytes,
      Err(response) => return response,
    };
    let payload: Value = match codec.decode(&bytes) {
      Ok(payload) => payload,
      Err(err) => {
        let detail = format!("malformed rpc input: {err}");
        return problem(StatusCode::BAD_REQUEST, detail, path).into_response();
      }
    };
    match kind {
      ProcedureKind::ServerStream => self.server_stream(entry, &parts, payload),
      _ => self.unary(entry, &parts, codec, None, tag, payload).await,
    }
  }

  /// `GET {path}/{tag}?input=<json>`: queries, with an ETag.
  async fn get_tag(&self, parts: Parts, tag: String) -> Response {
    let path = self.path();
    let version = match version_of(&parts.headers, path) {
      Ok(version) => version,
      Err(response) => return response,
    };
    let entry = match dispatch::resolve(self.server.group(), &tag, version) {
      Ok(entry) => entry,
      Err(rejection) => return self.rejection(rejection),
    };
    if entry.kind() != ProcedureKind::Query {
      return self.wrong_kind(&tag, entry.kind(), false);
    }
    if let Err(rejection) = dispatch::authorize(entry, &parts).await {
      return self.rejection(rejection);
    }
    let payload = match get_input(&parts) {
      Ok(payload) => payload,
      Err(detail) => return problem(StatusCode::BAD_REQUEST, detail, path).into_response(),
    };
    let codec = CodecId::Json;
    let outcome = match self.run_unary(entry, &parts, payload).await {
      Ok(outcome) => outcome,
      Err(response) => return response,
    };
    let status = outcome.status;
    let success = outcome.exit.is_success();
    let body = encode_result(codec, &outcome.into_result(None, tag));
    if !success {
      return json_response(status, codec, body);
    }
    let etag = etag_of(&body);
    let mut response = if none_match(&parts.headers, &etag) {
      StatusCode::NOT_MODIFIED.into_response()
    } else {
      json_response(status, codec, body)
    };
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&etag) {
      headers.insert(header::ETAG, value);
    }
    headers.insert(
      header::CACHE_CONTROL,
      HeaderValue::from_static("private, no-cache"),
    );
    response
  }

  async fn run_unary(
    &self,
    entry: &RpcMethodEntry,
    parts: &Parts,
    payload: Value,
  ) -> Result<CallOutcome, Response> {
    let run = dispatch::start_unary(entry, payload, self.ctx.clone(), self.control(parts))
      .map_err(|rejection| self.rejection(rejection))?;
    Ok(run.await)
  }

  /// Run a query or mutation and answer its `WireResult`; a mutation with an
  /// `Idempotency-Key` goes through the idempotency store.
  async fn unary(
    &self,
    entry: &RpcMethodEntry,
    parts: &Parts,
    codec: CodecId,
    id: Option<CallId>,
    tag: String,
    payload: Value,
  ) -> Response {
    if entry.kind() == ProcedureKind::Mutation
      && let Some(raw) = parts.headers.get(IDEMPOTENCY_KEY_HEADER)
    {
      let store = self.server.idempotency_store();
      return self
        .idempotent(entry, parts, codec, id, tag, payload, raw.as_bytes(), store)
        .await;
    }
    match self.run_unary(entry, parts, payload).await {
      Ok(outcome) => {
        let status = outcome.status;
        json_response(
          status,
          codec,
          encode_result(codec, &outcome.into_result(id, tag)),
        )
      }
      Err(response) => response,
    }
  }

  #[allow(clippy::too_many_arguments)]
  async fn idempotent(
    &self,
    entry: &RpcMethodEntry,
    parts: &Parts,
    codec: CodecId,
    id: Option<CallId>,
    tag: String,
    payload: Value,
    raw_key: &[u8],
    store: std::sync::Arc<dyn crate::idempotency::IdempotencyStore>,
  ) -> Response {
    let path = self.path();
    let principal = self.server.principal_of(parts);
    let key = match parse_key(raw_key, principal.is_none()) {
      Ok(key) => key,
      Err(err) => return coded(StatusCode::BAD_REQUEST, err.code(), err.to_string(), path),
    };
    let scope = match principal {
      Some(principal) => IdempotencyScope::Principal(principal),
      None => {
        let none = TrustedProxies::none();
        let trusted = parts.extensions.get::<TrustedProxies>().unwrap_or(&none);
        match ClientIp::from_parts(&parts.extensions, &parts.headers, trusted) {
          Some(ClientIp(ip)) => IdempotencyScope::Anonymous(ip),
          None => {
            let detail =
              "an anonymous Idempotency-Key needs the client address (serve with connect info)";
            return coded(
              StatusCode::BAD_REQUEST,
              "IdempotencyKeyUnscoped",
              detail,
              path,
            );
          }
        }
      }
    };
    let scoped_tag = format!("{tag}@{}", entry.version());
    // `serde_json::Value` maps are ordered, so equal payloads digest equally.
    let digest = payload_digest(
      &scoped_tag,
      &serde_json::to_vec(&payload).unwrap_or_default(),
    );
    let key = IdempotencyKey::new(scope, scoped_tag, key);
    match store.begin(&key, digest).await {
      Begin::Replay(stored) => {
        let exit: WireExit = serde_json::from_slice(&stored.body).unwrap_or(WireExit::Failure {
          error: crate::error_union::failure_value(&RpcFailure::InternalError {
            id: self.ctx.correlation_id.to_string(),
          }),
        });
        let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
        let mut response = json_response(
          status,
          codec,
          encode_result(codec, &WireResult::new(id, tag, exit)),
        );
        response
          .headers_mut()
          .insert(REPLAYED_HEADER, HeaderValue::from_static("true"));
        response
      }
      Begin::Mismatch => coded(
        StatusCode::CONFLICT,
        "IdempotencyKeyReused",
        "this Idempotency-Key was used with another payload",
        path,
      ),
      Begin::InProgress => coded(
        StatusCode::CONFLICT,
        "IdempotencyKeyInProgress",
        "a call with this Idempotency-Key is still running",
        path,
      ),
      Begin::Fresh => {
        let reservation = Reservation::new(store, key);
        let outcome = match self.run_unary(entry, parts, payload).await {
          Ok(outcome) => outcome,
          Err(response) => {
            reservation.finish(None).await;
            return response;
          }
        };
        let stored = outcome.is_replayable().then(|| StoredResult {
          digest,
          status: outcome.status.as_u16(),
          body: serde_json::to_vec(&outcome.exit).unwrap_or_default(),
        });
        reservation.finish(stored).await;
        let status = outcome.status;
        json_response(
          status,
          codec,
          encode_result(codec, &outcome.into_result(id, tag)),
        )
      }
    }
  }

  /// A server stream: NDJSON, one frame per line, `item`… then `exit`. Dropping the body (a
  /// client disconnect) drops the call.
  fn server_stream(&self, entry: &RpcMethodEntry, parts: &Parts, payload: Value) -> Response {
    let id = CallId::new(NDJSON_CALL_ID).unwrap_or(CallId::CONNECTION);
    let frames = match dispatch::start_server_stream(
      entry,
      id,
      payload,
      self.ctx.clone(),
      self.control(parts),
      self.server.stream_window(),
    ) {
      Ok(frames) => frames,
      Err(rejection) => return self.rejection(rejection),
    };
    let lines = frames.map(|frame| {
      // Frames of JSON values always encode.
      let mut line = serde_json::to_vec(&frame).unwrap_or_default();
      line.push(b'\n');
      Ok::<_, Infallible>(Bytes::from(line))
    });
    (
      StatusCode::OK,
      [(header::CONTENT_TYPE, NDJSON)],
      Body::from_stream(lines),
    )
      .into_response()
  }

  /// A client stream: NDJSON items read while the handler runs, under the item and byte caps.
  async fn client_stream(
    &self,
    entry: &RpcMethodEntry,
    parts: &Parts,
    body: Body,
    codec: CodecId,
    tag: String,
  ) -> Response {
    let (tx, run) = match dispatch::start_client_stream(
      entry,
      self.ctx.clone(),
      self.control(parts),
      self.server.stream_window(),
    ) {
      Ok(started) => started,
      Err(rejection) => return self.rejection(rejection),
    };
    let reader = read_ndjson(body, tx, entry.max_items(), entry.max_bytes());
    let outcome = match select(run, Box::pin(reader)).await {
      Either::Left((outcome, _reader)) => outcome,
      Either::Right((Ok(()), run)) => run.await,
      Either::Right((Err(outcome), run)) => {
        drop(run);
        outcome
      }
    };
    let status = outcome.status;
    json_response(
      status,
      codec,
      encode_result(codec, &outcome.into_result(None, tag)),
    )
  }
}

/// The `input` query parameter as JSON (`null` when absent).
fn get_input(parts: &Parts) -> Result<Value, String> {
  #[derive(serde::Deserialize)]
  struct Params {
    input: Option<String>,
  }
  let axum::extract::Query(params) = axum::extract::Query::<Params>::try_from_uri(&parts.uri)
    .map_err(|err| format!("malformed query string: {err}"))?;
  match params.input {
    None => Ok(Value::Null),
    Some(text) => serde_json::from_str(&text).map_err(|err| format!("malformed rpc input: {err}")),
  }
}

/// Feed the NDJSON lines of `body` to `tx`: `Ok` once the body ended (or the handler stopped
/// reading), or the failure that ends the call — a cap (`ResourceExhausted`), a line that is not
/// JSON (`InvalidInput`), a body that broke off (`Cancelled`).
async fn read_ndjson(
  body: Body,
  tx: InboundSender,
  max_items: u64,
  max_bytes: u64,
) -> Result<(), CallOutcome> {
  let mut stream = body.into_data_stream();
  let mut buffer: Vec<u8> = Vec::new();
  let mut bytes: u64 = 0;
  let mut items: u64 = 0;
  let mut ended = false;
  while !ended {
    match stream.next().await {
      Some(Ok(chunk)) => {
        bytes += chunk.len() as u64;
        if bytes > max_bytes {
          return Err(exhausted("max_bytes"));
        }
        buffer.extend_from_slice(&chunk);
      }
      Some(Err(_)) => return Err(CallOutcome::failure(RpcFailure::Cancelled)),
      None => {
        ended = true;
        buffer.push(b'\n');
      }
    }
    while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
      let line: Vec<u8> = buffer.drain(..=end).collect();
      let line = line.trim_ascii();
      if line.is_empty() {
        continue;
      }
      items += 1;
      if items > max_items {
        return Err(exhausted("max_items"));
      }
      let item: Value = serde_json::from_slice(line).map_err(|err| {
        CallOutcome::invalid_input(vec![InputIssue {
          path: String::new(),
          message: format!("item {items} is not JSON: {err}"),
        }])
      })?;
      if !tx.send(item).await {
        return Ok(());
      }
    }
    if buffer.len() > MAX_FRAME_BYTES {
      return Err(exhausted("max_frame"));
    }
  }
  Ok(())
}

fn exhausted(limit: &str) -> CallOutcome {
  CallOutcome::failure(RpcFailure::ResourceExhausted {
    limit: limit.to_owned(),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn etags_and_if_none_match() {
    let tag = etag_of(b"{}");
    assert_eq!(tag.len(), 34);
    let mut headers = HeaderMap::new();
    assert!(!none_match(&headers, &tag));
    headers.insert(
      header::IF_NONE_MATCH,
      HeaderValue::from_str(&format!("\"x\", W/{tag}")).unwrap(),
    );
    assert!(none_match(&headers, &tag));
    headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
    assert!(none_match(&headers, "\"other\""));
  }

  #[test]
  fn get_input_parses_json() {
    let parts = |uri: &str| Request::get(uri).body(()).unwrap().into_parts().0;
    assert_eq!(get_input(&parts("/rpc/A.b")), Ok(Value::Null));
    assert_eq!(
      get_input(&parts("/rpc/A.b?input=%7B%22a%22%3A1%7D")),
      Ok(serde_json::json!({"a": 1}))
    );
    assert!(
      get_input(&parts("/rpc/A.b?input=%7B"))
        .unwrap_err()
        .starts_with("malformed rpc input")
    );
    assert!(get_input(&parts("/rpc/A.b?input=1&input=2")).is_err());
  }

  #[tokio::test]
  async fn ndjson_reader_enforces_caps_and_reports_bad_lines() {
    let run = |body: &'static str, items: u64, bytes: u64| async move {
      let (tx, mut rx) = crate::handler::inbound(16);
      let out = read_ndjson(Body::from(body), tx, items, bytes).await;
      let mut got = Vec::new();
      while let Ok(v) = rx_try(&mut rx) {
        got.push(v);
      }
      (out, got)
    };
    let (out, got) = run("1\n\n2\n3", 10, 100).await;
    assert!(out.is_ok());
    assert_eq!(
      got,
      [
        serde_json::json!(1),
        serde_json::json!(2),
        serde_json::json!(3)
      ]
    );
    let (out, _) = run("1\n2\n3\n", 2, 100).await;
    assert_eq!(
      out.unwrap_err().reserved(),
      Some(RpcFailure::ResourceExhausted {
        limit: "max_items".into()
      })
    );
    let (out, _) = run("1\n2\n3\n", 10, 3).await;
    assert_eq!(
      out.unwrap_err().reserved(),
      Some(RpcFailure::ResourceExhausted {
        limit: "max_bytes".into()
      })
    );
    let (out, _) = run("{nope\n", 10, 100).await;
    assert!(matches!(
      out.unwrap_err().reserved(),
      Some(RpcFailure::InvalidInput { .. })
    ));
    let long = Box::leak("7".repeat(MAX_FRAME_BYTES + 1).into_boxed_str());
    let (out, _) = run(long, 10, u64::MAX).await;
    assert_eq!(
      out.unwrap_err().reserved(),
      Some(RpcFailure::ResourceExhausted {
        limit: "max_frame".into()
      })
    );
    let broken = Body::from_stream(futures_util::stream::iter([Err::<Bytes, _>(
      std::io::Error::other("reset"),
    )]));
    let (tx, _rx) = crate::handler::inbound(1);
    let out = read_ndjson(broken, tx, 10, 100).await;
    assert_eq!(out.unwrap_err().reserved(), Some(RpcFailure::Cancelled));
    let (tx, rx) = crate::handler::inbound(1);
    drop(rx);
    assert!(read_ndjson(Body::from("1\n2\n"), tx, 10, 100).await.is_ok());
  }

  fn rx_try(rx: &mut crate::handler::Inbound) -> Result<Value, ()> {
    rx.try_recv().ok_or(())
  }
}
