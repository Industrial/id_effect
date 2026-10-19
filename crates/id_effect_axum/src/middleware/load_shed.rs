//! Load shedding: [`LoadShedLayer`] caps the requests in flight and answers `503 Service
//! Unavailable` (Problem, `Retry-After`) at once when the cap is reached. Nothing queues: an
//! overloaded server rejects in constant time instead of buffering requests it cannot serve.
//!
//! A request counts from the moment the layer admits it until its response head is ready (or the
//! request is dropped, e.g. when the client disconnects); a streaming body keeps flowing after
//! that, bounded by the connection limits.
//!
//! The server entry points ([`server_layer`](crate::server_layer), [`serve`](crate::serve),
//! [`serve_router`](crate::serve_router) and the others) install it by default with the cap of
//! [`HostConfig::load_shed`](crate::HostConfig::load_shed) ([`LoadShed`]):
//! [`LoadShed::DEFAULT_PER_CPU`] requests per available CPU unless `MAX_IN_FLIGHT` says otherwise.
//! Turning it off is [`HostConfig::dangerous_no_load_shedding`](crate::HostConfig::dangerous_no_load_shedding).
//! A router served some other way adds the layer itself:
//!
//! ```rust
//! use axum::{Router, routing::get};
//! use id_effect_axum::middleware::LoadShedLayer;
//!
//! let app: Router = Router::new()
//!   .route("/", get(|| async { "hello" }))
//!   .layer(LoadShedLayer::new(1_024));
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tower_layer::Layer;
use tower_service::Service;

use super::problem_response;

/// The server's load-shedding setting ([`HostConfig::load_shed`](crate::HostConfig::load_shed)):
/// the in-flight cap and the `Retry-After` of a `503`. On by default; only
/// [`HostConfig::dangerous_no_load_shedding`](crate::HostConfig::dangerous_no_load_shedding)
/// turns it off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadShed {
  max_in_flight: Option<usize>,
  retry_after: Duration,
}

impl Default for LoadShed {
  /// [`Self::default_max_in_flight`] requests, `Retry-After: 1`.
  fn default() -> Self {
    Self::new(Self::default_max_in_flight())
  }
}

impl LoadShed {
  /// Default in-flight requests per available CPU: 512.
  pub const DEFAULT_PER_CPU: usize = 512;

  /// At most `max_in_flight` (at least 1) requests at once, `Retry-After: 1`.
  pub fn new(max_in_flight: usize) -> Self {
    Self {
      max_in_flight: Some(max_in_flight.max(1)),
      retry_after: Duration::from_secs(1),
    }
  }

  /// [`Self::DEFAULT_PER_CPU`] times [`std::thread::available_parallelism`] (one CPU when it is
  /// unknown).
  pub fn default_max_in_flight() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    cpus.saturating_mul(Self::DEFAULT_PER_CPU)
  }

  /// `Retry-After` of a `503`, rounded up to whole seconds (at least 1).
  #[must_use]
  pub fn retry_after(mut self, after: Duration) -> Self {
    self.retry_after = after;
    self
  }

  /// The in-flight cap; `None` when load shedding is off.
  pub fn max_in_flight(&self) -> Option<usize> {
    self.max_in_flight
  }

  /// Off: what [`HostConfig::dangerous_no_load_shedding`](crate::HostConfig::dangerous_no_load_shedding)
  /// sets (and records).
  pub(crate) fn disabled() -> Self {
    Self {
      max_in_flight: None,
      ..Self::default()
    }
  }

  /// The layer of this setting; `None` when load shedding is off.
  pub fn layer(&self) -> Option<LoadShedLayer> {
    self
      .max_in_flight
      .map(|max| LoadShedLayer::new(max).retry_after(self.retry_after))
  }

  /// `app` behind [`Self::layer`] (unchanged when load shedding is off).
  pub(crate) fn install(&self, app: Router) -> Router {
    match self.layer() {
      Some(layer) => app.layer(layer),
      None => app,
    }
  }
}

/// Answers `503` + `Retry-After` when `max_in_flight` requests are already in flight (see the
/// [module docs](self)). Clones share the count.
#[derive(Clone)]
pub struct LoadShedLayer {
  in_flight: Arc<AtomicUsize>,
  max_in_flight: usize,
  retry_after_secs: u64,
}

impl fmt::Debug for LoadShedLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LoadShedLayer")
      .field("in_flight", &self.in_flight())
      .field("max_in_flight", &self.max_in_flight)
      .field("retry_after_secs", &self.retry_after_secs)
      .finish()
  }
}

impl Default for LoadShedLayer {
  /// [`LoadShed::default_max_in_flight`] requests, `Retry-After: 1`.
  fn default() -> Self {
    Self::new(LoadShed::default_max_in_flight())
  }
}

impl LoadShedLayer {
  /// At most `max_in_flight` (at least 1) requests at once, `Retry-After: 1`.
  pub fn new(max_in_flight: usize) -> Self {
    Self {
      in_flight: Arc::new(AtomicUsize::new(0)),
      max_in_flight: max_in_flight.max(1),
      retry_after_secs: 1,
    }
  }

  /// `Retry-After` of a `503`, rounded up to whole seconds (at least 1).
  #[must_use]
  pub fn retry_after(mut self, after: Duration) -> Self {
    let secs = after.as_secs();
    let secs = if after.subsec_nanos() > 0 {
      secs.saturating_add(1)
    } else {
      secs
    };
    self.retry_after_secs = secs.max(1);
    self
  }

  /// The cap.
  pub fn max_in_flight(&self) -> usize {
    self.max_in_flight
  }

  /// Requests admitted and not yet answered.
  pub fn in_flight(&self) -> usize {
    self.in_flight.load(Ordering::Relaxed)
  }

  /// A slot, or `None` when every slot is taken.
  fn admit(&self) -> Option<InFlight> {
    let mut n = self.in_flight.load(Ordering::Acquire);
    loop {
      if n >= self.max_in_flight {
        return None;
      }
      match self
        .in_flight
        .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire)
      {
        Ok(_) => return Some(InFlight(Arc::clone(&self.in_flight))),
        Err(current) => n = current,
      }
    }
  }

  fn overloaded(&self, path: &str) -> Response {
    let secs = self.retry_after_secs;
    let mut response = problem_response(
      StatusCode::SERVICE_UNAVAILABLE,
      format!("server overloaded; retry after {secs} s"),
      path,
    );
    response
      .headers_mut()
      .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    response
  }
}

/// One admitted request; dropping it frees the slot.
struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
  fn drop(&mut self) {
    self.0.fetch_sub(1, Ordering::AcqRel);
  }
}

impl<S> Layer<S> for LoadShedLayer {
  type Service = LoadShedService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    LoadShedService {
      inner,
      config: self.clone(),
    }
  }
}

/// The [`Service`] produced by [`LoadShedLayer`].
#[derive(Clone, Debug)]
pub struct LoadShedService<S> {
  inner: S,
  config: LoadShedLayer,
}

impl<S> Service<Request> for LoadShedService<S>
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
    let Some(slot) = self.config.admit() else {
      let response = self.config.overloaded(request.uri().path());
      return Box::pin(std::future::ready(Ok(response)));
    };
    let response = self.inner.call(request);
    Box::pin(async move {
      let response = match response.await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
      };
      drop(slot);
      Ok(response)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn settings_default_on_and_clamp() {
    let shed = LoadShed::default();
    assert_eq!(
      shed.max_in_flight(),
      Some(LoadShed::default_max_in_flight())
    );
    assert!(LoadShed::default_max_in_flight() >= LoadShed::DEFAULT_PER_CPU);
    assert_eq!(LoadShed::new(0).max_in_flight(), Some(1));
    assert_eq!(LoadShed::disabled().max_in_flight(), None);
    assert!(LoadShed::disabled().layer().is_none());
    let layer = LoadShed::new(3)
      .retry_after(Duration::from_millis(2500))
      .layer()
      .unwrap();
    assert_eq!(layer.max_in_flight(), 3);
    assert_eq!(layer.retry_after_secs, 3);
    assert_eq!(LoadShedLayer::new(0).max_in_flight(), 1);
    assert_eq!(
      LoadShedLayer::new(1)
        .retry_after(Duration::ZERO)
        .retry_after_secs,
      1
    );
    assert_eq!(
      LoadShedLayer::default().max_in_flight(),
      LoadShed::default_max_in_flight()
    );
    assert!(format!("{layer:?}").contains("max_in_flight: 3"));
  }

  #[test]
  fn slots_are_freed_on_drop() {
    let layer = LoadShedLayer::new(2);
    let a = layer.admit().unwrap();
    let b = layer.admit().unwrap();
    assert!(layer.admit().is_none());
    assert_eq!(layer.in_flight(), 2);
    drop(a);
    let c = layer.admit().unwrap();
    drop((b, c));
    assert_eq!(layer.in_flight(), 0);
  }
}
