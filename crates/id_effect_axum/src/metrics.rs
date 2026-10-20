//! Prometheus scrape endpoint and automatic HTTP server metrics.
//!
//! - [`metrics_router`] / [`metrics_route`] serve `GET /metrics` in the Prometheus text format
//!   0.0.4 ([`prometheus::CONTENT_TYPE`]): the [`MetricRegistry`] snapshot plus the runtime
//!   metrics read at scrape time — `id_effect_fibers_live`, `id_effect_fibers_started_total`
//!   ([`runtime_metrics`]) and [`EXECUTOR_IN_FLIGHT`] (the global [`EffectExecutor`]).
//! - [`guarded_metrics_router`] serves the same route behind a [`RouteGuard`] (a scrape token,
//!   an operator role), for a router the public can reach. The metrics name every route and
//!   show the traffic: serve them unguarded only on an operator listener (ADR 0013 R7).
//! - [`http_metrics_layer`] records every request handled by the router it wraps:
//!   [`HTTP_SERVER_REQUESTS_TOTAL`]`{method,route,status}` and
//!   [`HTTP_SERVER_REQUEST_DURATION_SECONDS`]`{method,route}` (seconds, wall clock, until the
//!   response head is ready). `route` is the matched route template ([`MatchedPath`], e.g.
//!   `/users/{id}`), never the raw path: a request no route matched records under
//!   `route="`[`UNMATCHED_ROUTE`]`"`, so scanners cannot grow the series set. Non-standard
//!   methods record as `_OTHER`. A request whose future is dropped before it answers records
//!   nothing.
//!
//! ```rust
//! use axum::Router;
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::metrics::{http_metrics_layer, metrics_router};
//! use id_effect_axum::routing;
//!
//! fn hello(_: &mut ()) -> Effect<&'static str, std::convert::Infallible, ()> {
//!   succeed("hi")
//! }
//!
//! let app: Router = Router::new()
//!   .route("/hello", routing::get(hello))
//!   .merge(metrics_router())
//!   .layer(http_metrics_layer());
//! # let _ = app;
//! ```
//!
//! Use `Router::layer` (not `route_layer`) to count unmatched requests too.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::MatchedPath;
use axum::http::{Method, Request, Response, header};
use axum::response::IntoResponse;
use axum::routing::{MethodRouter, get};
use id_effect::observability::{prometheus, runtime_metrics};
use id_effect::{
  Boundaries, Counter, Histogram, Metric, MetricKey, MetricKind, MetricPair, MetricRegistry,
  MetricState,
};
use tower_layer::Layer;
use tower_service::Service;

use crate::EffectExecutor;
use crate::route::RouteGuard;

/// Counter: HTTP requests answered, by `method`, `route` and `status`.
pub const HTTP_SERVER_REQUESTS_TOTAL: &str = "http_server_requests_total";

/// Histogram: HTTP request duration in seconds, by `method` and `route`.
pub const HTTP_SERVER_REQUEST_DURATION_SECONDS: &str = "http_server_request_duration_seconds";

/// Gauge (sampled at scrape time): request fibers in flight on the global [`EffectExecutor`].
pub const EXECUTOR_IN_FLIGHT: &str = "id_effect_executor_in_flight";

/// The `route` label of a request no route matched.
pub const UNMATCHED_ROUTE: &str = "unmatched";

/// The duration buckets of [`HTTP_SERVER_REQUEST_DURATION_SECONDS`] (the Prometheus client
/// defaults, 5 ms to 10 s).
pub fn duration_boundaries() -> Boundaries {
  Boundaries::new([
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
  ])
}

/// The scrape body for `registry`: its snapshot plus the runtime metrics and
/// [`EXECUTOR_IN_FLIGHT`], read now.
pub fn render_metrics(registry: &MetricRegistry) -> String {
  let mut pairs = registry.snapshot();
  pairs.extend(runtime_metrics::snapshot());
  pairs.push(MetricPair {
    key: MetricKey::new(EXECUTOR_IN_FLIGHT, MetricKind::Gauge),
    state: MetricState::Gauge {
      value: i64::try_from(EffectExecutor::global().in_flight()).unwrap_or(i64::MAX),
    },
  });
  prometheus::render_pairs(&pairs)
}

/// `GET` scrape route over [`MetricRegistry::global`].
pub fn metrics_route<S: Clone + Send + Sync + 'static>() -> MethodRouter<S> {
  metrics_route_for(MetricRegistry::global().clone())
}

/// `GET` scrape route over `registry`.
pub fn metrics_route_for<S: Clone + Send + Sync + 'static>(
  registry: MetricRegistry,
) -> MethodRouter<S> {
  metrics_route_with(registry, || {})
}

/// `GET` scrape route over `registry` that runs `on_scrape` before each render (to sample
/// gauges such as a connection pool's into `registry`).
pub fn metrics_route_with<S: Clone + Send + Sync + 'static>(
  registry: MetricRegistry,
  on_scrape: impl Fn() + Send + Sync + 'static,
) -> MethodRouter<S> {
  let on_scrape = Arc::new(on_scrape);
  get(move || {
    let (registry, on_scrape) = (registry.clone(), Arc::clone(&on_scrape));
    async move {
      on_scrape();
      (
        [(header::CONTENT_TYPE, prometheus::CONTENT_TYPE)],
        render_metrics(&registry),
      )
        .into_response()
    }
  })
}

/// Router with `GET /metrics` over [`MetricRegistry::global`]. Unguarded: mount it on an
/// operator listener, or use [`guarded_metrics_router`] on a public one.
pub fn metrics_router<S: Clone + Send + Sync + 'static>() -> Router<S> {
  Router::new().route("/metrics", metrics_route())
}

/// [`metrics_router`] behind `guard`: a scrape the guard rejects gets the guard's answer
/// (typically `401` or `403`) and no metrics.
///
/// ```rust
/// use axum::body::Body;
/// use axum::http::request::Parts;
/// use axum::http::{Request, StatusCode};
/// use axum::response::IntoResponse;
/// use id_effect_axum::metrics::guarded_metrics_router;
/// use id_effect_axum::route::{GuardFuture, RouteGuard};
/// use tower::ServiceExt;
///
/// /// Admits scrapes carrying `authorization: Bearer scrape`.
/// struct ScrapeToken;
///
/// impl RouteGuard for ScrapeToken {
///   fn policy(&self) -> &str {
///     "metrics"
///   }
///
///   fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
///     let ok = parts.headers.get("authorization").is_some_and(|v| v == "Bearer scrape");
///     Box::pin(async move { if ok { Ok(()) } else { Err(StatusCode::UNAUTHORIZED.into_response()) } })
///   }
/// }
///
/// let app: axum::Router = guarded_metrics_router(ScrapeToken);
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let anonymous = Request::get("/metrics").body(Body::empty()).unwrap();
/// let answer = app.clone().oneshot(anonymous).await.unwrap();
/// assert_eq!(answer.status(), StatusCode::UNAUTHORIZED);
/// let scraper = Request::get("/metrics")
///   .header("authorization", "Bearer scrape")
///   .body(Body::empty())
///   .unwrap();
/// assert_eq!(app.oneshot(scraper).await.unwrap().status(), StatusCode::OK);
/// # });
/// ```
pub fn guarded_metrics_router<S: Clone + Send + Sync + 'static>(
  guard: impl RouteGuard,
) -> Router<S> {
  guard_routes(metrics_router(), Arc::new(guard))
}

/// Every route of `router` behind `guard` (a `route_layer`: unmatched requests are not checked).
pub(crate) fn guard_routes<S: Clone + Send + Sync + 'static>(
  router: Router<S>,
  guard: Arc<dyn RouteGuard>,
) -> Router<S> {
  router.route_layer(axum::middleware::from_fn(
    move |request: axum::extract::Request, next: axum::middleware::Next| {
      let guard = Arc::clone(&guard);
      async move {
        let (parts, body) = request.into_parts();
        let checked = guard.check(&parts).await;
        match checked {
          Ok(()) => {
            next
              .run(axum::extract::Request::from_parts(parts, body))
              .await
          }
          Err(rejection) => rejection,
        }
      }
    },
  ))
}

/// [`HttpMetricsLayer`] recording into [`MetricRegistry::global`].
pub fn http_metrics_layer() -> HttpMetricsLayer {
  HttpMetricsLayer::new(MetricRegistry::global().clone())
}

/// Tower layer recording [`HTTP_SERVER_REQUESTS_TOTAL`] and
/// [`HTTP_SERVER_REQUEST_DURATION_SECONDS`] (see the [module docs](self)).
#[derive(Clone, Debug)]
pub struct HttpMetricsLayer {
  metrics: HttpMetrics,
}

impl HttpMetricsLayer {
  /// A layer recording into `registry`.
  pub fn new(registry: MetricRegistry) -> Self {
    Self {
      metrics: HttpMetrics {
        requests: registry.counter(HTTP_SERVER_REQUESTS_TOTAL),
        duration: registry.histogram(HTTP_SERVER_REQUEST_DURATION_SECONDS, duration_boundaries()),
      },
    }
  }
}

impl<S> Layer<S> for HttpMetricsLayer {
  type Service = HttpMetricsService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    HttpMetricsService {
      inner,
      metrics: self.metrics.clone(),
    }
  }
}

#[derive(Clone, Debug)]
struct HttpMetrics {
  requests: Counter,
  duration: Histogram,
}

impl HttpMetrics {
  fn record(&self, method: &'static str, route: &str, status: u16, elapsed: Duration) {
    let (method, route) = (
      ("method".to_owned(), method.to_owned()),
      ("route".to_owned(), route.to_owned()),
    );
    self
      .duration
      .tagged_all(&[method.clone(), route.clone()])
      .update_now(elapsed.as_secs_f64());
    self
      .requests
      .tagged_all(&[method, route, ("status".to_owned(), status.to_string())])
      .update_now(1);
  }
}

/// The service [`HttpMetricsLayer`] wraps around a router's routes.
#[derive(Clone, Debug)]
pub struct HttpMetricsService<S> {
  inner: S,
  metrics: HttpMetrics,
}

/// A standard method's name, or `_OTHER` (bounded label values).
fn method_label(m: &Method) -> &'static str {
  match m.as_str() {
    "GET" => "GET",
    "POST" => "POST",
    "PUT" => "PUT",
    "DELETE" => "DELETE",
    "PATCH" => "PATCH",
    "HEAD" => "HEAD",
    "OPTIONS" => "OPTIONS",
    "CONNECT" => "CONNECT",
    "TRACE" => "TRACE",
    _ => "_OTHER",
  }
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<S, B, ResBody> Service<Request<B>> for HttpMetricsService<S>
where
  S: Service<Request<B>, Response = Response<ResBody>>,
  S::Future: Send + 'static,
{
  type Response = Response<ResBody>;
  type Error = S::Error;
  type Future = BoxFuture<Result<Response<ResBody>, S::Error>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    let method = method_label(req.method());
    let route = req
      .extensions()
      .get::<MatchedPath>()
      .map_or(UNMATCHED_ROUTE, MatchedPath::as_str)
      .to_owned();
    let start = Instant::now();
    let fut = self.inner.call(req);
    let metrics = self.metrics.clone();
    Box::pin(async move {
      let res = fut.await;
      if let Ok(r) = &res {
        metrics.record(method, &route, r.status().as_u16(), start.elapsed());
      }
      res
    })
  }
}
