//! Request limits: [`TimeoutLayer`] (whole-request deadline → `503`) and [`BodyLimitLayer`]
//! (request body size → `413`), both answering with a [`Problem`](crate::Problem).
//!
//! **Timeout.** When the deadline passes the inner service's future is dropped. Under the
//! default [`DropPolicy::Interrupt`](crate::DropPolicy::Interrupt) that interrupts the handler's
//! request fiber, so its finalizers (`ensuring`, `acquire_release`, scoped layers) run and no
//! work outlives the response.
//!
//! **Body limit.** A `Content-Length` above the limit is refused before the handler runs; a body
//! without one (chunked) is cut off when it grows past the limit, which axum's body extractors
//! (`Bytes`, `String`, `Json`, the typed routes) report as `413`, rewritten here into a Problem.
//! The limit also replaces axum's built-in 2 MiB default for those extractors.
//!
//! ```rust
//! use std::time::Duration;
//!
//! use axum::{Router, routing::post};
//! use id_effect_axum::middleware::{BodyLimitLayer, TimeoutLayer};
//!
//! let app: Router = Router::new()
//!   .route("/upload", post(|body: String| async move { body.len().to_string() }))
//!   .layer(TimeoutLayer::new(Duration::from_secs(5)))
//!   .layer(BodyLimitLayer::new(16 * 1024 * 1024));
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::Limited;
use tower_layer::Layer;
use tower_service::Service;

use super::problem_response;
use crate::route::PROBLEM_JSON;

/// [`TimeoutLayer::default`]: 30 seconds.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// [`BodyLimitLayer::default`]: 2 MiB (axum's own default).
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Answers `503 Service Unavailable` (Problem, `Retry-After: 1`) when a request takes longer
/// than its deadline, interrupting the handler (see the [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeoutLayer {
  timeout: Duration,
}

impl Default for TimeoutLayer {
  fn default() -> Self {
    Self::new(DEFAULT_TIMEOUT)
  }
}

impl TimeoutLayer {
  /// Deadline `timeout` for the whole request (inner layers and handler).
  pub fn new(timeout: Duration) -> Self {
    Self { timeout }
  }

  /// The configured deadline.
  pub fn timeout(&self) -> Duration {
    self.timeout
  }
}

impl<S> Layer<S> for TimeoutLayer {
  type Service = TimeoutService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    TimeoutService {
      inner,
      timeout: self.timeout,
    }
  }
}

/// The [`Service`] produced by [`TimeoutLayer`].
#[derive(Clone, Debug)]
pub struct TimeoutService<S> {
  inner: S,
  timeout: Duration,
}

impl<S> Service<Request> for TimeoutService<S>
where
  S: Service<Request, Error = Infallible>,
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
    let path = request.uri().path().to_owned();
    let timeout = self.timeout;
    let call = self.inner.call(request);
    Box::pin(async move {
      let response = match tokio::time::timeout(timeout, call).await {
        Ok(Ok(response)) => response.into_response(),
        Ok(Err(never)) => match never {},
        Err(_elapsed) => {
          let mut response = problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
              "the request did not complete within {} ms",
              timeout.as_millis()
            ),
            &path,
          );
          response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
          response
        }
      };
      Ok(response)
    })
  }
}

/// Answers `413 Content Too Large` (Problem) for a request body above the limit (see the
/// [module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyLimitLayer {
  limit: usize,
}

impl Default for BodyLimitLayer {
  fn default() -> Self {
    Self::new(DEFAULT_BODY_LIMIT)
  }
}

impl BodyLimitLayer {
  /// Accept request bodies of at most `limit` bytes.
  pub fn new(limit: usize) -> Self {
    Self { limit }
  }

  /// The configured limit in bytes.
  pub fn limit(&self) -> usize {
    self.limit
  }
}

impl<S> Layer<S> for BodyLimitLayer {
  type Service = BodyLimitService<<DefaultBodyLimit as Layer<S>>::Service>;

  fn layer(&self, inner: S) -> Self::Service {
    BodyLimitService {
      // axum's body extractors read this limit instead of their 2 MiB default.
      inner: DefaultBodyLimit::max(self.limit).layer(inner),
      limit: self.limit,
    }
  }
}

/// The [`Service`] produced by [`BodyLimitLayer`] (`S` is the inner service with axum's
/// extractor limit set).
#[derive(Clone, Debug)]
pub struct BodyLimitService<S> {
  inner: S,
  limit: usize,
}

impl<S> BodyLimitService<S> {
  fn too_large(&self, path: &str) -> Response {
    problem_response(
      StatusCode::PAYLOAD_TOO_LARGE,
      format!("the request body exceeds {} bytes", self.limit),
      path,
    )
  }
}

/// The declared `Content-Length`, when present and numeric.
fn content_length(request: &Request) -> Option<u64> {
  request
    .headers()
    .get(header::CONTENT_LENGTH)?
    .to_str()
    .ok()?
    .parse()
    .ok()
}

/// Whether `response` is a `413` that is not already a Problem (axum's plain-text rejection).
fn plain_too_large(response: &Response) -> bool {
  response.status() == StatusCode::PAYLOAD_TOO_LARGE
    && response
      .headers()
      .get(header::CONTENT_TYPE)
      .is_none_or(|ct| ct.as_bytes() != PROBLEM_JSON.as_bytes())
}

impl<S> Service<Request> for BodyLimitService<S>
where
  S: Service<Request, Error = Infallible>,
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
    let path = request.uri().path().to_owned();
    if content_length(&request).is_some_and(|len| len > self.limit as u64) {
      let response = self.too_large(&path);
      return Box::pin(async move { Ok(response) });
    }
    let too_large = self.too_large(&path);
    let request = request.map(|body| Body::new(Limited::new(body, self.limit)));
    let call = self.inner.call(request);
    Box::pin(async move {
      let response = match call.await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
      };
      Ok(if plain_too_large(&response) {
        too_large
      } else {
        response
      })
    })
  }
}
