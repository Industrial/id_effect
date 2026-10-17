//! Request correlation: [`RequestIdLayer`] gives every request a [`RequestId`] and runs it inside
//! one effect span.
//!
//! - **Id.** A valid incoming `x-request-id` (1–128 characters of `A-Z a-z 0-9 - _ . : + / = @`)
//!   is kept, otherwise a random one is generated. The id is written back to the request header,
//!   stored as a request extension (handlers extract [`RequestId`]) and echoed on the response.
//! - **Span.** The rest of the stack and the handler run in a fiber under an
//!   [`Effect::with_span_attrs`] span named [`REQUEST_SPAN`] with the attributes
//!   `http.request.method`, `http.route` (the matched route template, else the path) and
//!   `request_id`, annotated with `http.response.status_code` when the response is ready. The
//!   span is reported to a [`SpanHook`]: the in-process tracing buffer ([`id_effect::tracing_hook`])
//!   by default, or the OpenTelemetry bridge with [`RequestIdLayer::span_hook`]. Dropping the
//!   request (client gone, a timeout) interrupts that fiber and with it the handler's.
//!
//! ```rust
//! use axum::{Router, routing::get};
//! use id_effect_axum::middleware::{RequestId, RequestIdLayer};
//!
//! async fn show(id: RequestId) -> String {
//!   format!("request {id}")
//! }
//!
//! let app: Router = Router::new()
//!   .route("/", get(show))
//!   .layer(RequestIdLayer::new());
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{FromRequestParts, MatchedPath, OptionalFromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use id_effect::{Effect, HNil, SpanHook, annotate_current_span, from_async, tracing_hook};
use tower_layer::Layer;
use tower_service::Service;

use crate::executor::EffectExecutor;
use crate::route::Problem;
use crate::server::security::random_token;

/// The default request-id header.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Name of the span [`RequestIdLayer`] opens around each request.
pub const REQUEST_SPAN: &str = "http.request";

/// Longest incoming request id that is kept.
const MAX_LEN: usize = 128;

/// A request's correlation id (see the [module docs](self)); extract it in a handler, or read it
/// from the request extensions.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(Arc<str>);

impl RequestId {
  /// `id` when it is a valid request id (1–128 characters of `A-Z a-z 0-9 - _ . : + / = @`).
  pub fn parse(id: &str) -> Option<Self> {
    let valid = !id.is_empty()
      && id.len() <= MAX_LEN
      && id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.:+/=@".contains(&b));
    valid.then(|| Self(Arc::from(id)))
  }

  /// A fresh random id (128 bits, base64url).
  pub fn generate() -> Self {
    Self(Arc::from(random_token(16)))
  }

  /// The id text.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for RequestId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestId {
  /// `500` Problem: the [`RequestIdLayer`] is not installed on this route.
  type Rejection = Problem;

  async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
    parts.extensions.get::<RequestId>().cloned().ok_or_else(|| {
      Problem::new(StatusCode::INTERNAL_SERVER_ERROR)
        .with_detail("RequestIdLayer is not installed on this route")
    })
  }
}

impl<S: Send + Sync> OptionalFromRequestParts<S> for RequestId {
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Option<Self>, Infallible> {
    Ok(parts.extensions.get::<RequestId>().cloned())
  }
}

/// Assigns a [`RequestId`] and opens the [`REQUEST_SPAN`] span per request (see the
/// [module docs](self)).
#[derive(Clone)]
pub struct RequestIdLayer {
  header: HeaderName,
  trust_incoming: bool,
  hook: Arc<dyn SpanHook>,
  executor: EffectExecutor,
}

impl fmt::Debug for RequestIdLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("RequestIdLayer")
      .field("header", &self.header)
      .field("trust_incoming", &self.trust_incoming)
      .finish_non_exhaustive()
  }
}

impl Default for RequestIdLayer {
  fn default() -> Self {
    Self::new()
  }
}

impl RequestIdLayer {
  /// `x-request-id`, incoming ids trusted, spans reported to [`id_effect::tracing_hook`].
  pub fn new() -> Self {
    Self {
      header: HeaderName::from_static(REQUEST_ID_HEADER),
      trust_incoming: true,
      hook: tracing_hook(),
      // Its own unbounded executor: the span fiber must not take an in-flight slot of the
      // global one, which the handler's fiber needs.
      executor: EffectExecutor::new(),
    }
  }

  /// Read and write the id in `header` instead of `x-request-id`.
  #[must_use]
  pub fn header(mut self, header: HeaderName) -> Self {
    self.header = header;
    self
  }

  /// Keep a valid incoming id (`true`, default) or always generate one (an edge service facing
  /// untrusted clients).
  #[must_use]
  pub fn trust_incoming(mut self, trust: bool) -> Self {
    self.trust_incoming = trust;
    self
  }

  /// Report the request span to `hook` (e.g. the OpenTelemetry span bridge).
  #[must_use]
  pub fn span_hook(mut self, hook: Arc<dyn SpanHook>) -> Self {
    self.hook = hook;
    self
  }

  fn request_id(&self, request: &Request) -> RequestId {
    self
      .trust_incoming
      .then(|| request.headers().get(&self.header))
      .flatten()
      .and_then(|v| v.to_str().ok())
      .and_then(RequestId::parse)
      .unwrap_or_else(RequestId::generate)
  }

  async fn handle<S>(self: Arc<Self>, mut inner: S, mut request: Request) -> Response
  where
    S: Service<Request, Error = Infallible>,
    S::Response: IntoResponse,
    S::Future: Send + 'static,
  {
    let id = self.request_id(&request);
    // Valid ids are visible ASCII, so they are always valid header values.
    let value = HeaderValue::from_str(id.as_str()).expect("request ids are header-safe");
    request
      .headers_mut()
      .insert(self.header.clone(), value.clone());
    request.extensions_mut().insert(id.clone());
    let route = request
      .extensions()
      .get::<MatchedPath>()
      .map_or_else(|| request.uri().path(), MatchedPath::as_str)
      .to_owned();
    let attributes = [
      ("http.request.method", request.method().as_str().to_owned()),
      ("http.route", route),
      ("request_id", id.to_string()),
    ];
    let call = inner.call(request);
    let eff: Effect<Response, Infallible, HNil> = from_async(move || async move {
      match call.await {
        Ok(response) => Ok(response.into_response()),
        Err(never) => match never {},
      }
    })
    .flat_map(|response: Response| {
      let status = response.status().as_u16().to_string();
      annotate_current_span("http.response.status_code", status).map(move |()| response)
    })
    .with_span_attrs(REQUEST_SPAN, attributes)
    .with_span_hook(Arc::clone(&self.hook));
    let mut response = crate::exit_response(self.executor.run_effect(eff).await);
    response.headers_mut().insert(self.header.clone(), value);
    response
  }
}

impl<S> Layer<S> for RequestIdLayer {
  type Service = RequestIdService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    RequestIdService {
      inner,
      config: Arc::new(self.clone()),
    }
  }
}

/// The [`Service`] produced by [`RequestIdLayer`].
#[derive(Clone, Debug)]
pub struct RequestIdService<S> {
  inner: S,
  config: Arc<RequestIdLayer>,
}

impl<S> Service<Request> for RequestIdService<S>
where
  S: Service<Request, Error = Infallible> + Clone + Send + 'static,
  S::Response: IntoResponse,
  S::Future: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request) -> Self::Future {
    // The ready service handles this request; the clone waits for the next `poll_ready`.
    let clone = self.inner.clone();
    let inner = std::mem::replace(&mut self.inner, clone);
    let config = Arc::clone(&self.config);
    Box::pin(async move { Ok(config.handle(inner, request).await) })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_accepts_only_header_safe_ids() {
    assert_eq!(
      RequestId::parse("abc-123_x.y:z").unwrap().as_str(),
      "abc-123_x.y:z"
    );
    assert!(RequestId::parse("").is_none());
    assert!(RequestId::parse("has space").is_none());
    assert!(RequestId::parse("new\nline").is_none());
    assert!(RequestId::parse(&"a".repeat(MAX_LEN)).is_some());
    assert!(RequestId::parse(&"a".repeat(MAX_LEN + 1)).is_none());
  }

  #[test]
  fn generated_ids_are_valid_and_distinct() {
    let a = RequestId::generate();
    assert!(RequestId::parse(a.as_str()).is_some());
    assert_ne!(a, RequestId::generate());
    assert_eq!(a.to_string(), a.as_str());
  }

  #[test]
  fn layer_debug_names_the_header() {
    let layer = RequestIdLayer::default().header(HeaderName::from_static("x-correlation-id"));
    assert!(format!("{layer:?}").contains("x-correlation-id"));
  }
}
