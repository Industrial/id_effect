//! The Streamable HTTP transport (`feature = "mcp-http"`): [`McpEndpoint`], an `id_effect_axum`
//! route that answers `POST` with the server's response.
//!
//! Secure by default (ADR 0013, ADR 0015 B16):
//!
//! - **An access decision is required (R1, R7).** An endpoint is [`Unguarded`] until
//!   [`McpEndpoint::guard`] attaches a route guard; only a [`Guarded`] endpoint has a
//!   [`router`](McpEndpoint::router), so an MCP endpoint without an access decision does not
//!   compile. The principal the guard admitted (`id_effect_authz`'s `CurrentPrincipal`) is the
//!   one handles and cursors are bound to; an anonymous principal is refused (`401`).
//! - **Origin allowlist (R9).** A request carrying `Origin` must match the endpoint's
//!   [`OriginPolicy`] (empty by default: every browser is refused, the DNS-rebinding defence);
//!   requests without `Origin` (non-browser clients) pass.
//! - **Finite (R4).** A body over [`McpLimits::max_message_bytes`](super::McpLimits) (1 MiB) is
//!   refused with `413` before it is parsed; each principal has at most
//!   [`McpLimits::max_in_flight`](super::McpLimits) requests running (`429` beyond).
//! - **Headers agree with the body.** `MCP-Protocol-Version` must name the pinned revision, and
//!   `Mcp-Method`, `Mcp-Name` and `Mcp-Param-*` must match the JSON-RPC body (`400`,
//!   `HeaderMismatch`).
//!
//! A request whose `_meta` carries a `progressToken` is answered as server-sent events (its
//! progress notifications, then its response); any other as one `application/json` body.
//! Closing the response (a client disconnect) interrupts the request's fiber. Notifications are
//! accepted with `202`.
//!
//! ```compile_fail
//! use id_effect_ai::mcp::{McpEndpoint, McpServer};
//!
//! // No guard: there is no router to mount.
//! let _ = McpEndpoint::new(McpServer::new("s", "1")).router::<()>();
//! ```

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect_authz::guard::principal_of;
use id_effect_axum::realtime::origin::OriginPolicy;
use id_effect_axum::route::{Endpoint, GuardFuture, RouteGuard, RouteInfo, Router};
use serde_json::Value;
use tokio::sync::mpsc;

use super::context::ProgressSink;
use super::headers::{HEADER_PROTOCOL_VERSION, check_request_headers};
use super::message::{
  DecodeError, HEADER_MISMATCH, Message, RequestId, RpcError, SERVER_BUSY,
  decode_message_with_limit,
};
use super::server::{McpServer, unsupported_version};
use super::{META_PROTOCOL_VERSION, NOTIFICATION_PROGRESS, PROTOCOL_VERSION};

/// The default path of an MCP endpoint.
pub const DEFAULT_PATH: &str = "/mcp";

mod sealed {
  pub trait Sealed {}
}

/// The access state of an [`McpEndpoint`]: [`Unguarded`] or [`Guarded`]. Sealed.
pub trait EndpointAccess: sealed::Sealed + Send + Sync + 'static {}

/// No guard yet: the endpoint cannot be mounted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unguarded;

/// At least one guard decides before the endpoint ([`McpEndpoint::guard`]).
#[derive(Clone)]
pub struct Guarded {
  first: Arc<dyn RouteGuard>,
  more: Vec<Arc<dyn RouteGuard>>,
}

impl Guarded {
  /// The guards' policy names, in the order they were added.
  pub fn policies(&self) -> impl Iterator<Item = &str> {
    std::iter::once(&self.first)
      .chain(&self.more)
      .map(|g| g.policy())
  }
}

impl fmt::Debug for Guarded {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_list().entries(self.policies()).finish()
  }
}

impl sealed::Sealed for Unguarded {}
impl sealed::Sealed for Guarded {}
impl EndpointAccess for Unguarded {}
impl EndpointAccess for Guarded {}

/// One shared guard, attached to the endpoint.
struct SharedGuard(Arc<dyn RouteGuard>);

impl RouteGuard for SharedGuard {
  fn policy(&self) -> &str {
    self.0.policy()
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    self.0.check(parts)
  }
}

/// An MCP server mounted as a Streamable HTTP route (see the [module docs](self)).
///
/// ```
/// use id_effect_ai::mcp::{McpEndpoint, McpServer};
/// use id_effect_authz::{Guard, require_role};
/// use id_effect_axum::realtime::origin::OriginPolicy;
///
/// let router = McpEndpoint::new(McpServer::new("docs", "1.0.0"))
///   .allow_origins(OriginPolicy::allow(["https://app.example.com"]).unwrap())
///   .guard(Guard::new("mcp", require_role("agent")))
///   .router::<()>();
/// let report = router.access_report();
/// assert!(report.public.is_empty());
/// assert_eq!(report.guarded.len(), 1);
/// ```
pub struct McpEndpoint<A: EndpointAccess = Unguarded> {
  server: McpServer,
  path: String,
  origins: OriginPolicy,
  access: A,
}

impl<A: EndpointAccess + fmt::Debug> fmt::Debug for McpEndpoint<A> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("McpEndpoint")
      .field("path", &self.path)
      .field("origins", &self.origins.entries())
      .field("access", &self.access)
      .finish_non_exhaustive()
  }
}

impl McpEndpoint<Unguarded> {
  /// `server` at [`DEFAULT_PATH`], refusing every browser origin, without a guard yet.
  pub fn new(server: McpServer) -> Self {
    Self {
      server,
      path: DEFAULT_PATH.to_owned(),
      origins: OriginPolicy::deny_all(),
      access: Unguarded,
    }
  }

  /// The endpoint checks `guard` before every request (an `id_effect_authz::Guard` answers 401
  /// to the anonymous principal and 403 to a principal its policy denies).
  pub fn guard(self, guard: impl RouteGuard) -> McpEndpoint<Guarded> {
    McpEndpoint {
      server: self.server,
      path: self.path,
      origins: self.origins,
      access: Guarded {
        first: Arc::new(guard),
        more: Vec::new(),
      },
    }
  }
}

impl<A: EndpointAccess> McpEndpoint<A> {
  /// Serves at `path` instead of [`DEFAULT_PATH`].
  #[must_use]
  pub fn at(mut self, path: impl Into<String>) -> Self {
    self.path = path.into();
    self
  }

  /// The browser origins admitted (empty by default; see `OriginPolicy`).
  #[must_use]
  pub fn allow_origins(mut self, origins: OriginPolicy) -> Self {
    self.origins = origins;
    self
  }

  /// The path served.
  pub fn path(&self) -> &str {
    &self.path
  }
}

impl McpEndpoint<Guarded> {
  /// Also check `guard` (outside the guards added before).
  #[must_use]
  pub fn guard(mut self, guard: impl RouteGuard) -> Self {
    self.access.more.push(Arc::new(guard));
    self
  }

  /// The guards, innermost first.
  pub fn guards(&self) -> &Guarded {
    &self.access
  }

  /// The endpoint as a typed router with one guarded `POST` route (merge it into the
  /// application's router).
  pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
    let shared = Arc::new(Shared {
      server: self.server,
      origins: self.origins,
      in_flight: Mutex::default(),
    });
    let method_router = axum::routing::post(move |req: Request| handle(Arc::clone(&shared), req));
    let mut info = RouteInfo::new(Method::POST, self.path);
    info.handler = Some("id_effect_ai::mcp::McpEndpoint");
    let Guarded { first, more } = self.access;
    let endpoint =
      Endpoint::<S, _>::from_parts(info, method_router.with_state(())).guard(SharedGuard(first));
    let endpoint = more
      .into_iter()
      .fold(endpoint, |e, g| e.guard(SharedGuard(g)));
    Router::new().route(endpoint)
  }
}

struct Shared {
  server: McpServer,
  origins: OriginPolicy,
  in_flight: Mutex<HashMap<Arc<str>, usize>>,
}

/// One in-flight slot of a principal, released on drop.
struct Slot {
  shared: Arc<Shared>,
  principal: Arc<str>,
}

impl Slot {
  fn take(shared: &Arc<Shared>, principal: &Arc<str>) -> Option<Self> {
    let max = shared.server.limits().max_in_flight;
    let mut map = shared
      .in_flight
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let n = map.entry(Arc::clone(principal)).or_insert(0);
    if *n >= max {
      return None;
    }
    *n += 1;
    Some(Self {
      shared: Arc::clone(shared),
      principal: Arc::clone(principal),
    })
  }
}

impl Drop for Slot {
  fn drop(&mut self) {
    let mut map = self
      .shared
      .in_flight
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(n) = map.get_mut(&self.principal) {
      *n -= 1;
      if *n == 0 {
        map.remove(&self.principal);
      }
    }
  }
}

/// A JSON-RPC error answered with `status`.
fn rpc_error(status: StatusCode, id: Option<RequestId>, error: RpcError) -> Response {
  json_response(status, Message::error(id, error).to_text())
}

fn json_response(status: StatusCode, body: String) -> Response {
  let mut res = (status, body).into_response();
  res.headers_mut().insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("application/json"),
  );
  res
}

async fn handle(shared: Arc<Shared>, req: Request) -> Response {
  let (parts, body) = req.into_parts();
  if let Err(rejection) = shared.origins.check(&parts.headers) {
    return rejection.into_response();
  }
  let principal = principal_of(&parts);
  if !principal.is_authenticated() {
    return StatusCode::UNAUTHORIZED.into_response();
  }
  let principal: Arc<str> = Arc::from(principal.id());
  let max = shared.server.limits().max_message_bytes;
  let too_large = || {
    rpc_error(
      StatusCode::PAYLOAD_TOO_LARGE,
      None,
      DecodeError::TooLarge { max }.to_rpc_error(),
    )
  };
  let declared = parts
    .headers
    .get(header::CONTENT_LENGTH)
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.parse::<usize>().ok());
  if declared.is_some_and(|n| n > max) {
    return too_large();
  }
  let Ok(bytes) = axum::body::to_bytes(body, max).await else {
    return too_large();
  };
  let msg = match decode_message_with_limit(&bytes, max) {
    Ok(msg) => msg,
    Err(e) => return rpc_error(StatusCode::BAD_REQUEST, None, e.to_rpc_error()),
  };
  let id = match &msg {
    Message::Request(r) => Some(r.id.clone()),
    _ => None,
  };
  let header = |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok());
  match header(HEADER_PROTOCOL_VERSION) {
    Some(PROTOCOL_VERSION) => {}
    Some(other) => {
      return rpc_error(StatusCode::BAD_REQUEST, id, unsupported_version(other));
    }
    None => {
      return rpc_error(
        StatusCode::BAD_REQUEST,
        id,
        RpcError::new(HEADER_MISMATCH, "missing MCP-Protocol-Version header"),
      );
    }
  }
  let body_version = match &msg {
    Message::Request(r) => r
      .params
      .as_ref()
      .and_then(|p| p.get("_meta"))
      .and_then(|m| m.get(META_PROTOCOL_VERSION))
      .and_then(Value::as_str),
    _ => None,
  };
  let schema = match &msg {
    Message::Request(r) if r.method == "tools/call" => r
      .params
      .as_ref()
      .and_then(|p| p.get("name"))
      .and_then(Value::as_str)
      .and_then(|name| {
        shared
          .server
          .tools()
          .definitions()
          .into_iter()
          .find(|d| d.name == name)
      })
      .map(|d| d.parameters),
    _ => None,
  };
  let mismatch = match body_version {
    Some(v) if v != PROTOCOL_VERSION => {
      Err("MCP-Protocol-Version header does not match the body".to_owned())
    }
    _ => check_request_headers(header, &msg, schema.as_ref()),
  };
  if let Err(why) = mismatch {
    return rpc_error(
      StatusCode::BAD_REQUEST,
      id,
      RpcError::new(HEADER_MISMATCH, why),
    );
  }
  let Message::Request(request) = msg else {
    // Notifications and responses: accepted, nothing to answer.
    return StatusCode::ACCEPTED.into_response();
  };
  let Some(slot) = Slot::take(&shared, &principal) else {
    return rpc_error(
      StatusCode::TOO_MANY_REQUESTS,
      Some(request.id),
      RpcError::new(SERVER_BUSY, "too many requests in flight"),
    );
  };
  let wants_progress = request
    .params
    .as_ref()
    .and_then(|p| p.get("_meta"))
    .and_then(|m| m.get("progressToken"))
    .is_some();
  if wants_progress {
    return stream_response(&shared.server, request, principal, slot);
  }
  let running = shared.server.start(request, principal, None);
  let out = running.await;
  drop(slot);
  match out {
    Some(msg) => json_response(StatusCode::OK, msg.to_text()),
    None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
  }
}

/// Aborts the task driving a streamed request when the response body is dropped (the client
/// went away), which drops the request and interrupts its fiber.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
  fn drop(&mut self) {
    self.0.abort();
  }
}

fn sse_event(msg: &Message) -> Bytes {
  Bytes::from(format!("event: message\ndata: {}\n\n", msg.to_text()))
}

/// Answers `request` as server-sent events: its progress notifications, then its response.
fn stream_response(
  server: &McpServer,
  request: super::message::Request,
  principal: Arc<str>,
  slot: Slot,
) -> Response {
  let (tx, rx) = mpsc::channel::<Option<Bytes>>(64);
  let progress_tx = tx.clone();
  let sink: ProgressSink = Arc::new(move |params| {
    let note = Message::notification(NOTIFICATION_PROGRESS, Some(params));
    // Progress is lossy: a slow reader drops notifications rather than block the tool.
    let _ = progress_tx.try_send(Some(sse_event(&note)));
  });
  let running = server.start(request, principal, Some(sink));
  let task = tokio::spawn(async move {
    let out = running.await;
    if let Some(msg) = out {
      let _ = tx.send(Some(sse_event(&msg))).await;
    }
    let _ = tx.send(None).await;
    drop(slot);
  });
  let guard = AbortOnDrop(task);
  let stream = futures_util::stream::unfold((rx, guard), |(mut rx, guard)| async move {
    match rx.recv().await {
      Some(Some(bytes)) => Some((Ok::<_, Infallible>(bytes), (rx, guard))),
      _ => None,
    }
  });
  let mut res = Response::new(Body::from_stream(stream));
  let headers = res.headers_mut();
  headers.insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("text/event-stream"),
  );
  headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
  res
}
