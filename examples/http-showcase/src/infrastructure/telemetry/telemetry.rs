//! [`Telemetry`]: where request spans and the latency histogram go.

use std::convert::Infallible;
use std::sync::OnceLock;

use axum::Router;
use axum::middleware;
use axum::response::Response;
use id_effect::{Boundaries, Effect, Histogram, Metric, MetricRegistry};
use id_effect_opentelemetry::{OtelConfig, OtelSpanHook, OtelStarterGuard, install_from_config};
use opentelemetry::Context;

use super::track_request;

/// Where request spans and the latency histogram go (see the [module docs](super)). Cheap to
/// clone.
#[derive(Clone)]
pub struct Telemetry {
  latency: Histogram,
  spans: Option<OtelSpanHook>,
  /// Keeps the OTLP providers and the global registry bridge alive (from [`Self::from_env`]).
  guard: Option<OtelStarterGuard>,
}

impl std::fmt::Debug for Telemetry {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Telemetry")
      .field("exports_spans", &self.spans.is_some())
      .field("otlp", &self.guard.is_some())
      .finish_non_exhaustive()
  }
}

impl Telemetry {
  /// The latency histogram's name (OpenTelemetry HTTP semantic conventions), in seconds.
  pub const REQUEST_DURATION: &'static str = "http.server.request.duration";

  /// The span / histogram attribute carrying the matched route template.
  pub const ROUTE: &'static str = "http.route";

  /// The span / histogram attribute carrying the request method.
  pub const METHOD: &'static str = "http.request.method";

  /// The `http.route` value of a request no route matched (the fallback).
  pub const UNMATCHED: &'static str = "unmatched";

  /// The environment variable that turns export on ([`Self::from_env`]).
  pub const ENDPOINT_VAR: &'static str = "OTEL_EXPORTER_OTLP_ENDPOINT";

  /// The instrumentation scope / default `service.name`.
  pub const SERVICE_NAME: &'static str = "http_showcase";

  /// Bucket boundaries (seconds) recommended for [`Self::REQUEST_DURATION`].
  pub const DURATION_BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
  ];

  /// Requests timed into `registry`'s [`Self::REQUEST_DURATION`] histogram (bridge the
  /// registry to export it); spans exported through `spans` when given.
  pub fn new(registry: &MetricRegistry, spans: Option<OtelSpanHook>) -> Self {
    Self {
      latency: registry.histogram(
        Self::REQUEST_DURATION,
        Boundaries::new(Self::DURATION_BUCKETS),
      ),
      spans,
      guard: None,
    }
  }

  /// No export: the histogram records into [`MetricRegistry::global`], spans go nowhere.
  pub fn disabled() -> Self {
    Self::new(MetricRegistry::global(), None)
  }

  /// Exports through an installed OTLP starter: spans on its tracer provider, the histogram in
  /// [`MetricRegistry::global`] (which the starter bridges). The guard is kept alive.
  pub fn from_guard(guard: OtelStarterGuard) -> Self {
    let spans = OtelSpanHook::for_provider(&guard.providers().tracer, Self::SERVICE_NAME);
    Self {
      guard: Some(guard),
      ..Self::new(MetricRegistry::global(), Some(spans))
    }
  }

  /// [`Self::from_guard`] on [`install_from_config`]`(`[`OtelConfig::from_env`]`)` when
  /// [`Self::ENDPOINT_VAR`] is set and not blank, else [`Self::disabled`]. A configuration or
  /// exporter error is reported on stderr and also falls back to [`Self::disabled`]: telemetry
  /// never stops the service from starting.
  pub fn from_env() -> Self {
    match std::env::var(Self::ENDPOINT_VAR) {
      Ok(endpoint) if !endpoint.trim().is_empty() => {}
      _ => return Self::disabled(),
    }
    let installed = OtelConfig::from_env().and_then(|mut config| {
      if std::env::var_os("OTEL_SERVICE_NAME").is_none() {
        config.service_name = Self::SERVICE_NAME.into();
      }
      install_from_config(config)
    });
    match installed {
      Ok(guard) => Self::from_guard(guard),
      Err(e) => {
        eprintln!("http_showcase: telemetry disabled ({e})");
        Self::disabled()
      }
    }
  }

  /// The process-wide telemetry ([`Self::from_env`] on first use).
  pub fn global() -> &'static Telemetry {
    static GLOBAL: OnceLock<Telemetry> = OnceLock::new();
    GLOBAL.get_or_init(Self::from_env)
  }

  /// Whether request spans are exported.
  pub fn exports_spans(&self) -> bool {
    self.spans.is_some()
  }

  /// The latency histogram (untagged handle; series are tagged per route and method).
  pub fn latency(&self) -> &Histogram {
    &self.latency
  }

  /// Flushes pending OTLP batches (no-op without an exporter). Call before the process exits.
  pub fn flush(&self) {
    if let Some(guard) = &self.guard {
      guard.force_flush();
    }
  }

  /// `router` with every request (fallback included) recorded: one span, one histogram
  /// observation.
  pub fn instrument(&self, router: Router) -> Router {
    router.layer(middleware::from_fn_with_state(self.clone(), track_request))
  }

  /// The effect serving one request: `inner` timed and spanned, the span parented under
  /// `parent` when there is one.
  pub(crate) fn request_effect(
    &self,
    method: String,
    route: String,
    parent: Option<Context>,
    inner: Effect<Response, Infallible>,
  ) -> Effect<Response, Infallible> {
    let timed = self
      .latency
      .tagged(Self::ROUTE, route.clone())
      .tagged(Self::METHOD, method.clone())
      .track_duration(inner);
    let spanned = timed.with_span_attrs(
      format!("{method} {route}"),
      [(Self::ROUTE, route), (Self::METHOD, method)],
    );
    match (&self.spans, parent) {
      (Some(hook), Some(parent)) => {
        spanned.with_span_hook(hook.clone().with_parent(parent).into_hook())
      }
      (Some(hook), None) => spanned.with_span_hook(hook.clone().into_hook()),
      (None, _) => spanned,
    }
  }
}

#[cfg(test)]
mod tests {
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use axum::routing::get;
  use id_effect::succeed;
  use id_effect_opentelemetry::{
    bridge_metrics, install_w3c_propagators, sdk_meter_provider_with_in_memory_exporter,
    sdk_tracer_provider_with_in_memory_exporter,
  };
  use opentelemetry::KeyValue;
  use opentelemetry::metrics::MeterProvider as _;
  use opentelemetry::trace::{Status, TraceContextExt as _, Tracer as _, TracerProvider as _};
  use opentelemetry_sdk::metrics::InMemoryMetricExporter;
  use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
  use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
  use tower::ServiceExt as _;

  use super::*;

  const ROUTE: &str = Telemetry::ROUTE;
  const METHOD: &str = Telemetry::METHOD;
  const UNMATCHED: &str = Telemetry::UNMATCHED;

  /// A router like the showcase's: a plain axum route, a templated route and an effect handler.
  fn app() -> Router {
    Router::new()
      .route("/health", get(|| async { "ok" }))
      .route("/todos/{id}", get(|| async { "todo" }))
      .route(
        "/effect",
        id_effect_axum::routing::get(|_: &mut ()| {
          succeed::<&'static str, Infallible, ()>("effect").with_span("handler.inner")
        }),
      )
  }

  struct Harness {
    spans: InMemorySpanExporter,
    metrics: InMemoryMetricExporter,
    tracer: opentelemetry_sdk::trace::SdkTracerProvider,
    meter: opentelemetry_sdk::metrics::SdkMeterProvider,
    _bridge: id_effect_opentelemetry::MetricsBridge,
    app: Router,
  }

  /// `app()` instrumented against in-memory span and metric exporters (own registry).
  fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let metrics = InMemoryMetricExporter::default();
    let tracer = sdk_tracer_provider_with_in_memory_exporter(&spans);
    let meter = sdk_meter_provider_with_in_memory_exporter(&metrics);
    let registry = MetricRegistry::new();
    let bridge = bridge_metrics(&registry, meter.meter("test"));
    let hook = OtelSpanHook::for_provider(&tracer, "test");
    let app = Telemetry::new(&registry, Some(hook)).instrument(app());
    Harness {
      spans,
      metrics,
      tracer,
      meter,
      _bridge: bridge,
      app,
    }
  }

  async fn call(app: &Router, request: Request<Body>) -> StatusCode {
    app
      .clone()
      .oneshot(request)
      .await
      .expect("infallible")
      .status()
  }

  fn get_req(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
  }

  fn attr(attrs: &[KeyValue], key: &str) -> Option<String> {
    attrs
      .iter()
      .find(|kv| kv.key.as_str() == key)
      .map(|kv| kv.value.as_str().into_owned())
  }

  /// `(route, method, count)` per exported data point of [`Telemetry::REQUEST_DURATION`].
  fn histogram_points(metrics: &InMemoryMetricExporter) -> Vec<(String, String, u64)> {
    let finished = metrics.get_finished_metrics().expect("metrics");
    let last = finished.last().expect("one export");
    let metric = last
      .scope_metrics()
      .flat_map(|s| s.metrics())
      .find(|m| m.name() == Telemetry::REQUEST_DURATION)
      .expect("duration histogram exported");
    let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data() else {
      panic!("expected an f64 histogram, got {:?}", metric.data());
    };
    let mut points: Vec<_> = h
      .data_points()
      .map(|p| {
        let attrs: Vec<KeyValue> = p.attributes().cloned().collect();
        (
          attr(&attrs, ROUTE).unwrap_or_default(),
          attr(&attrs, METHOD).unwrap_or_default(),
          p.count(),
        )
      })
      .collect();
    points.sort();
    points
  }

  fn routes_of(spans: &[SpanData]) -> Vec<String> {
    let mut routes: Vec<String> = spans
      .iter()
      .map(|s| attr(&s.attributes, ROUTE).expect("every span has http.route"))
      .collect();
    routes.sort();
    routes
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn each_request_records_one_span_and_one_observation() {
    let h = harness();
    for path in ["/health", "/todos/1", "/todos/2", "/effect", "/missing"] {
      call(&h.app, get_req(path)).await;
    }

    let spans = h.spans.get_finished_spans().expect("spans");
    assert_eq!(spans.len(), 5, "one span per request: {spans:?}");
    assert_eq!(
      routes_of(&spans),
      [
        "/effect",
        "/health",
        "/todos/{id}",
        "/todos/{id}",
        UNMATCHED
      ]
    );
    let todo = spans
      .iter()
      .find(|s| s.name == "GET /todos/{id}")
      .expect("span named after method and route");
    assert_eq!(attr(&todo.attributes, METHOD).as_deref(), Some("GET"));
    assert_eq!(todo.status, Status::Ok);

    h.meter.force_flush().expect("flush");
    let points = histogram_points(&h.metrics);
    assert_eq!(
      points,
      [
        ("/effect".into(), "GET".into(), 1),
        ("/health".into(), "GET".into(), 1),
        ("/todos/{id}".into(), "GET".into(), 2),
        (UNMATCHED.into(), "GET".into(), 1),
      ]
    );
    assert_eq!(points.iter().map(|p| p.2).sum::<u64>(), 5);
    let _ = h.tracer.shutdown();
    let _ = h.meter.shutdown();
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn responses_pass_through_unchanged() {
    let h = harness();
    assert_eq!(call(&h.app, get_req("/health")).await, StatusCode::OK);
    assert_eq!(call(&h.app, get_req("/effect")).await, StatusCode::OK);
    assert_eq!(
      call(&h.app, get_req("/missing")).await,
      StatusCode::NOT_FOUND
    );
    let response = h.app.clone().oneshot(get_req("/todos/7")).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024)
      .await
      .unwrap();
    assert_eq!(&body[..], b"todo");
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_w3c_traceparent_becomes_the_span_parent() {
    install_w3c_propagators();
    let h = harness();
    let remote = Context::new().with_span(h.tracer.tracer("remote").start("client"));
    let mut headers = Vec::new();
    id_effect_opentelemetry::inject_trace_context_into_headers(&remote, &mut headers);
    let mut request = Request::builder().uri("/health");
    for (k, v) in &headers {
      request = request.header(k, v);
    }
    call(&h.app, request.body(Body::empty()).unwrap()).await;
    let spans = h.spans.get_finished_spans().expect("spans");
    let server = spans
      .iter()
      .find(|s| s.name == "GET /health")
      .expect("server span");
    let remote_span = remote.span().span_context().clone();
    assert_eq!(server.span_context.trace_id(), remote_span.trace_id());
    assert_eq!(server.parent_span_id, remote_span.span_id());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn without_an_exporter_requests_are_only_timed() {
    let registry = MetricRegistry::new();
    let telemetry = Telemetry::new(&registry, None);
    assert!(!telemetry.exports_spans());
    let app = telemetry.instrument(app());
    assert_eq!(call(&app, get_req("/health")).await, StatusCode::OK);
    let series = telemetry
      .latency()
      .tagged(ROUTE, "/health")
      .tagged(METHOD, "GET");
    assert_eq!(series.snapshot().count, 1);
    telemetry.flush();
  }

  #[test]
  fn from_env_without_an_endpoint_is_disabled() {
    // The test process does not set OTEL_EXPORTER_OTLP_ENDPOINT.
    if std::env::var_os(Telemetry::ENDPOINT_VAR).is_none() {
      let telemetry = Telemetry::from_env();
      assert!(!telemetry.exports_spans());
      assert!(format!("{telemetry:?}").contains("otlp: false"));
    }
  }
}
