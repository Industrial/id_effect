//! [`Console`] and its guarded JSON routes under [`API_PREFIX`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use id_effect::runtime_metrics;
use id_effect::{Exit, FiberInfo, FiberState, MetricPair, MetricRegistry, MetricState, fiber_dump};
use id_effect_authz::Guard;
use id_effect_axum::{EffectExecutor, HealthRegistry};
use id_effect_process::SupervisorHandle;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::log_level::{self, EnvFilterHandle, LogLevelChange, LogLevelError};
use crate::supervisors;

/// Where the console API is mounted: every route is `/_console/api/<name>`.
pub const API_PREFIX: &str = "/_console/api";

/// How long a `which_children` call may take before the supervisor is reported unreachable
/// ([`Console::call_timeout`]).
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// What the console shows, built up then turned into its guarded routes by [`Console::router`].
///
/// ```
/// use id_effect_authz::{Guard, require_role};
/// use id_effect_axum::HealthRegistry;
/// use id_effect_console::Console;
///
/// let routes: axum::Router = Console::new()
///   .health(HealthRegistry::new())
///   .router(Guard::new("console", require_role("operator")));
/// # let _ = routes;
/// ```
#[derive(Clone)]
pub struct Console {
  supervisors: Vec<(String, SupervisorHandle)>,
  health: HealthRegistry,
  metrics: MetricRegistry,
  env_filter: Option<EnvFilterHandle>,
  call_timeout: Duration,
}

impl Default for Console {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Debug for Console {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let supervisors: Vec<&str> = self.supervisors.iter().map(|(id, _)| id.as_str()).collect();
    f.debug_struct("Console")
      .field("supervisors", &supervisors)
      .field("health", &self.health)
      .field("env_filter", &self.env_filter.is_some())
      .field("call_timeout", &self.call_timeout)
      .finish_non_exhaustive()
  }
}

impl Console {
  /// A console over the process: every live fiber, the global [`MetricRegistry`], an empty
  /// (always up) [`HealthRegistry`], no supervisors and no reloadable `EnvFilter`.
  pub fn new() -> Self {
    Self {
      supervisors: Vec::new(),
      health: HealthRegistry::new(),
      metrics: MetricRegistry::global().clone(),
      env_filter: None,
      call_timeout: DEFAULT_CALL_TIMEOUT,
    }
  }

  /// Shows `handle`'s tree under the name `id`. Register nested supervisors too (by the handle
  /// the app holds): one whose pid is a registered supervisor's child is shown inside its tree.
  #[must_use]
  pub fn supervisor(mut self, id: impl Into<String>, handle: SupervisorHandle) -> Self {
    self.supervisors.push((id.into(), handle));
    self
  }

  /// The health checks `GET health` runs.
  #[must_use]
  pub fn health(mut self, registry: HealthRegistry) -> Self {
    self.health = registry;
    self
  }

  /// The registry `GET metrics` reads (default: [`MetricRegistry::global`]).
  #[must_use]
  pub fn metrics(mut self, registry: MetricRegistry) -> Self {
    self.metrics = registry;
    self
  }

  /// The reload handle of the subscriber's `EnvFilter` that `log-level` reads and changes
  /// (`id_effect_opentelemetry`'s `OtelStarterGuard::env_filter`). Without one, `log-level`
  /// changes only the `EffectLogger` minimum.
  #[must_use]
  pub fn env_filter(mut self, handle: EnvFilterHandle) -> Self {
    self.env_filter = Some(handle);
    self
  }

  /// How long `GET supervisors` waits for each supervisor's `which_children` (default 2 s).
  #[must_use]
  pub fn call_timeout(mut self, timeout: Duration) -> Self {
    self.call_timeout = timeout;
    self
  }

  /// The console routes under [`API_PREFIX`], every one of them (and the console router's
  /// fallback) behind `guard`. Merge the result into the app's router, inside its
  /// authentication layer.
  pub fn router(self, guard: Guard) -> Router {
    let api = Router::new()
      .route("/fibers", get(fibers))
      .route("/supervisors", get(supervisors_tree))
      .route("/metrics", get(metrics))
      .route("/health", get(health))
      .route("/log-level", get(log_level_get).put(log_level_put))
      .fallback(not_found)
      .layer(axum::middleware::map_response(no_store))
      .with_state(Arc::new(self));
    Router::new().nest(API_PREFIX, api.layer(guard))
  }
}

type Shared = State<Arc<Console>>;

/// One live fiber, as `GET /_console/api/fibers` lists it (by ascending id).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiberView {
  /// The fiber's id.
  pub id: u64,
  /// The forking fiber, while it is alive.
  pub parent: Option<u64>,
  /// Children forked with `Effect::fork` that have not completed yet.
  pub children: Vec<u64>,
  /// `running`, `interrupting` or `done` (completed while the dump was taken).
  pub state: String,
  /// Milliseconds since the fiber was built.
  pub age_ms: u64,
  /// Span names from the current span outwards.
  pub spans: Vec<String>,
}

impl From<FiberInfo> for FiberView {
  fn from(info: FiberInfo) -> Self {
    let state = match info.state {
      FiberState::Running => "running",
      FiberState::Interrupting => "interrupting",
      FiberState::Done => "done",
      _ => "unknown",
    };
    Self {
      id: info.id.as_u64(),
      parent: info.parent.map(|p| p.as_u64()),
      children: info.children.iter().map(|c| c.as_u64()).collect(),
      state: state.to_owned(),
      age_ms: u64::try_from(info.age.as_millis()).unwrap_or(u64::MAX),
      spans: info.spans,
    }
  }
}

/// One metric series, as `GET /_console/api/metrics` lists it: the registry's series sorted by
/// name, kind and tags, then the runtime's `id_effect_fibers_live` and
/// `id_effect_fibers_started_total`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricView {
  /// The metric name.
  pub name: String,
  /// `counter`, `gauge`, `histogram`, `summary` or `frequency`.
  pub kind: String,
  /// The series' tags.
  pub tags: BTreeMap<String, String>,
  /// The reading: `{count}` (counter), `{value}` (gauge), `{count, sum, min, max, buckets:
  /// [{le, count}]}` (histogram; the last `le` is `null`, +∞), `{count, sum, min, max,
  /// window_len, quantiles: [{q, value}]}` (summary) or `{occurrences: {value: count}}`
  /// (frequency).
  pub value: Value,
}

impl From<MetricPair> for MetricView {
  fn from(pair: MetricPair) -> Self {
    let value = match pair.state {
      MetricState::Counter { count } => json!({ "count": count }),
      MetricState::Gauge { value } => json!({ "value": value }),
      MetricState::Histogram(h) => json!({
        "count": h.count,
        "sum": h.sum,
        "min": h.min,
        "max": h.max,
        "buckets": h
          .buckets
          .iter()
          .map(|(le, count)| json!({ "le": le.is_finite().then_some(*le), "count": count }))
          .collect::<Vec<_>>(),
      }),
      MetricState::Summary(s) => json!({
        "count": s.count,
        "sum": s.sum,
        "min": s.min,
        "max": s.max,
        "window_len": s.window_len,
        "quantiles": s
          .quantiles
          .iter()
          .map(|(q, value)| json!({ "q": q, "value": value }))
          .collect::<Vec<_>>(),
      }),
      MetricState::Frequency { occurrences } => json!({
        "occurrences": occurrences.into_iter().collect::<BTreeMap<_, _>>(),
      }),
    };
    Self {
      name: pair.key.name().to_owned(),
      kind: pair.key.kind().label().to_owned(),
      tags: pair.key.tags().iter().cloned().collect(),
      value,
    }
  }
}

async fn fibers() -> Json<Vec<FiberView>> {
  Json(fiber_dump().into_iter().map(FiberView::from).collect())
}

async fn supervisors_tree(State(console): Shared) -> Json<supervisors::SupervisorsView> {
  Json(supervisors::snapshot(&console.supervisors, console.call_timeout).await)
}

async fn metrics(State(console): Shared) -> Json<Vec<MetricView>> {
  let series = console.metrics.snapshot().into_iter();
  Json(
    series
      .chain(runtime_metrics::snapshot())
      .map(MetricView::from)
      .collect(),
  )
}

/// The health report, `200` whatever its status: the console shows a down service, it does not
/// probe it (that is `/ready`).
async fn health(State(console): Shared) -> Response {
  match EffectExecutor::global()
    .run_effect(console.health.check())
    .await
  {
    Exit::Success(report) => Json(report).into_response(),
    Exit::Failure(cause) => problem(
      StatusCode::INTERNAL_SERVER_ERROR,
      format!("health check did not complete: {cause:?}"),
    ),
  }
}

async fn log_level_get(State(console): Shared) -> Json<log_level::LogLevelView> {
  Json(log_level::current(console.env_filter.as_ref()))
}

async fn log_level_put(
  State(console): Shared,
  body: Result<Json<LogLevelChange>, JsonRejection>,
) -> Response {
  let Json(change) = match body {
    Ok(body) => body,
    Err(rejection) => return problem(StatusCode::BAD_REQUEST, rejection.body_text()),
  };
  match log_level::apply(console.env_filter.as_ref(), change) {
    Ok(view) => Json(view).into_response(),
    Err(LogLevelError::Invalid(detail)) => problem(StatusCode::BAD_REQUEST, detail),
    Err(LogLevelError::Reload(detail)) => problem(StatusCode::INTERNAL_SERVER_ERROR, detail),
  }
}

async fn not_found() -> Response {
  problem(StatusCode::NOT_FOUND, "no such console route")
}

async fn no_store(mut response: Response) -> Response {
  response
    .headers_mut()
    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
  response
}

/// A Problem Details (RFC 9457) answer, as the guard's denials are.
fn problem(status: StatusCode, detail: impl Into<String>) -> Response {
  let body = json!({
    "type": "about:blank",
    "title": status.canonical_reason().unwrap_or("Error"),
    "status": status.as_u16(),
    "detail": detail.into(),
  });
  let mut response = (status, body.to_string()).into_response();
  response.headers_mut().insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("application/problem+json"),
  );
  response
}
