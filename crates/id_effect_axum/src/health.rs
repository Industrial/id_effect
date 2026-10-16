//! Liveness (`/health`) and readiness (`/ready`) route helpers for Axum services.
//!
//! - [`health`] / [`ready`] / [`ready_with_state`]: plain-text probes over a predicate or a
//!   [`ReadinessState`] flip-switch.
//! - [`HealthRegistry`]: aggregated readiness. Each [`HealthIndicator`] wraps an [`Effect`] that
//!   probes one dependency and reports a [`HealthStatus`] (`up` / `degraded` / `down`) with a JSON
//!   detail. The checks run concurrently, each bounded by its own timeout (a timed-out check is
//!   `down`), and `/ready` answers `{status, checks: {name: {status, detail, duration_ms}}}` with
//!   `200`, or `503` when any check (or the registry's [`ReadinessState`]) is down.
//!
//! The host's shutdown paths ([`HostBuilder::run_until_shutdown`](crate::HostBuilder::run_until_shutdown),
//! [`serve_router`](crate::serve_router), [`ServerHandle::shutdown`](crate::ServerHandle::shutdown))
//! call [`ReadinessState::mark_shutting_down`] before draining, so `/ready` reports `503` with
//! reason `shutting_down` while in-flight requests finish.

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use id_effect::{Cache, Cause, Concurrency, Effect, Exit, HNil, Never, fork_all, suspend, sync};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Async readiness predicate; return `true` when the service can accept traffic.
pub type ReadinessCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Shared flip-switch for readiness (for example set `false` during drain, `true` after startup).
#[derive(Clone, Debug, Default)]
pub struct ReadinessState {
  ready: Arc<AtomicBool>,
  shutting_down: Arc<AtomicBool>,
}

impl ReadinessState {
  /// Creates state with an initial readiness flag.
  pub fn new(initial: bool) -> Self {
    Self {
      ready: Arc::new(AtomicBool::new(initial)),
      shutting_down: Arc::new(AtomicBool::new(false)),
    }
  }

  /// Updates whether the service accepts traffic (`true` also clears a shutting-down mark).
  pub fn set_ready(&self, ready: bool) {
    if ready {
      self.shutting_down.store(false, Ordering::SeqCst);
    }
    self.ready.store(ready, Ordering::SeqCst);
  }

  /// Current readiness flag.
  pub fn is_ready(&self) -> bool {
    self.ready.load(Ordering::SeqCst)
  }

  /// Marks the service not ready because it is shutting down (reason `shutting_down`). The host's
  /// shutdown paths call it first, before draining in-flight work.
  pub fn mark_shutting_down(&self) {
    self.shutting_down.store(true, Ordering::SeqCst);
    self.ready.store(false, Ordering::SeqCst);
  }

  /// Whether [`mark_shutting_down`](Self::mark_shutting_down) was called (and not undone by
  /// `set_ready(true)`).
  pub fn is_shutting_down(&self) -> bool {
    self.shutting_down.load(Ordering::SeqCst)
  }

  /// Why the service is not ready: `None` when ready, `shutting_down` during shutdown,
  /// `not_ready` otherwise.
  pub fn not_ready_reason(&self) -> Option<&'static str> {
    if self.is_ready() {
      None
    } else if self.is_shutting_down() {
      Some("shutting_down")
    } else {
      Some("not_ready")
    }
  }
}

async fn health_handler() -> impl IntoResponse {
  (StatusCode::OK, "ok")
}

async fn ready_handler(check: ReadinessCheck) -> impl IntoResponse {
  if check() {
    (StatusCode::OK, "ready")
  } else {
    (StatusCode::SERVICE_UNAVAILABLE, "not ready")
  }
}

async fn ready_state_handler(state: ReadinessState) -> impl IntoResponse {
  if state.is_ready() {
    (StatusCode::OK, "ready")
  } else {
    (StatusCode::SERVICE_UNAVAILABLE, "not ready")
  }
}

/// `GET` liveness route — always 200 when the process is running.
pub fn health() -> MethodRouter {
  get(health_handler)
}

/// `GET` readiness route driven by `check`.
pub fn ready(check: ReadinessCheck) -> MethodRouter {
  get(move || {
    let check = check.clone();
    async move { ready_handler(check).await }
  })
}

/// `GET` readiness route using [`ReadinessState`].
pub fn ready_with_state(state: ReadinessState) -> MethodRouter {
  get(move || {
    let state = state.clone();
    async move { ready_state_handler(state).await }
  })
}

/// Router with `/health` only.
pub fn health_router() -> Router {
  Router::new().route("/health", health())
}

/// Router with `/ready` only.
pub fn readiness_router(check: ReadinessCheck) -> Router {
  Router::new().route("/ready", ready(check))
}

/// Router with `/health` and `/ready`.
pub fn observability_routes(check: ReadinessCheck) -> Router {
  Router::new()
    .route("/health", health())
    .route("/ready", ready(check))
}

/// Router with `/health` and `/ready` backed by [`ReadinessState`].
pub fn observability_routes_with_state(state: ReadinessState) -> Router {
  Router::new()
    .route("/health", health())
    .route("/ready", ready_with_state(state))
}

/// Middleware that returns **503** when [`ReadinessState`] is not ready.
pub async fn require_ready(
  State(state): State<ReadinessState>,
  request: Request,
  next: Next,
) -> Response {
  if state.is_ready() {
    next.run(request).await
  } else {
    (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
  }
}

/// Timeout a [`HealthIndicator`] starts with ([`HealthIndicator::with_timeout`] changes it).
pub const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// A health check's verdict. Ordered by severity: `Up` < `Degraded` < `Down`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
  /// Healthy.
  Up,
  /// Working, but impaired; still ready (`/ready` answers `200`).
  Degraded,
  /// Not healthy; `/ready` answers `503`.
  Down,
}

impl HealthStatus {
  /// The more severe of `self` and `other`.
  pub fn worst(self, other: Self) -> Self {
    self.max(other)
  }
}

/// What one check reports: a [`HealthStatus`] and a JSON detail (`null` when there is none).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HealthOutcome {
  /// The verdict.
  pub status: HealthStatus,
  /// Free-form detail (pool sizes, error message, …).
  pub detail: Value,
}

impl HealthOutcome {
  /// `Up` without detail.
  pub fn up() -> Self {
    Self {
      status: HealthStatus::Up,
      detail: Value::Null,
    }
  }

  /// `Degraded` with `detail`.
  pub fn degraded(detail: impl Into<Value>) -> Self {
    Self {
      status: HealthStatus::Degraded,
      detail: detail.into(),
    }
  }

  /// `Down` with `detail`.
  pub fn down(detail: impl Into<Value>) -> Self {
    Self {
      status: HealthStatus::Down,
      detail: detail.into(),
    }
  }

  /// Replaces the detail.
  pub fn with_detail(mut self, detail: impl Into<Value>) -> Self {
    self.detail = detail.into();
    self
  }
}

/// One check's entry in a [`HealthReport`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CheckReport {
  /// The verdict (`Down` when the check failed, died or timed out).
  pub status: HealthStatus,
  /// The check's detail; `{"error": ..}` when it failed, died or timed out.
  pub detail: Value,
  /// Wall time the check took, timeout included.
  pub duration_ms: u64,
}

type CheckFn = Arc<dyn Fn() -> Effect<HealthOutcome, String> + Send + Sync>;

/// One named health check: builds the [`Effect`] that probes a dependency, afresh on every probe.
///
/// A typed failure, a defect or running past [`timeout`](Self::timeout) reports
/// [`HealthStatus::Down`] with detail `{"error": ..}`.
#[derive(Clone)]
pub struct HealthIndicator {
  name: String,
  check: CheckFn,
  timeout: Duration,
}

impl fmt::Debug for HealthIndicator {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("HealthIndicator")
      .field("name", &self.name)
      .field("timeout", &self.timeout)
      .finish_non_exhaustive()
  }
}

fn millis(d: Duration) -> u64 {
  u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn failure_message(cause: Cause<String>) -> String {
  match crate::split_cause(cause) {
    Ok(e) => e,
    Err(rest) => crate::first_defect(&rest).map_or_else(|| "interrupted".to_owned(), str::to_owned),
  }
}

impl HealthIndicator {
  /// An indicator named `name` whose check is the effect `check()` builds.
  pub fn new<E>(
    name: impl Into<String>,
    check: impl Fn() -> Effect<HealthOutcome, E> + Send + Sync + 'static,
  ) -> Self
  where
    E: fmt::Display + Send + 'static,
  {
    Self {
      name: name.into(),
      check: Arc::new(move || check().map_err(|e| e.to_string())),
      timeout: DEFAULT_CHECK_TIMEOUT,
    }
  }

  /// An indicator over a plain effect: success is `Up` with the value as detail (`()` gives
  /// `null`), a failure is `Down` with the error.
  pub fn from_effect<A, E>(
    name: impl Into<String>,
    check: impl Fn() -> Effect<A, E> + Send + Sync + 'static,
  ) -> Self
  where
    A: Serialize + Send + 'static,
    E: fmt::Display + Send + 'static,
  {
    Self::new(name, move || {
      check().map(|a| HealthOutcome::up().with_detail(serde_json::to_value(a).unwrap_or_default()))
    })
  }

  /// Adapts a [`ReadinessCheck`] predicate: `true` is `Up`, `false` is `Down`.
  pub fn from_readiness(name: impl Into<String>, check: ReadinessCheck) -> Self {
    Self::new(name, move || {
      let check = Arc::clone(&check);
      sync::<_, Never, HNil>(move || {
        if check() {
          HealthOutcome::up()
        } else {
          HealthOutcome::down(json!({ "error": "not ready" }))
        }
      })
    })
  }

  /// Reports a [`Cache`]'s [`stats`](Cache::stats) (`hits`, `misses`, `size`) as an `Up` check.
  pub fn cache<K, V, E, R>(name: impl Into<String>, cache: Cache<K, V, E, R>) -> Self
  where
    K: Clone + Eq + Hash + Send + 'static,
    V: Clone + Send + 'static,
    E: Clone + Send + 'static,
    R: Send + Sync + 'static,
    Cache<K, V, E, R>: Send + Sync,
  {
    Self::new(name, move || {
      cache.stats::<Never, HNil>().map(|s| {
        HealthOutcome::up().with_detail(json!({
          "hits": s.hits,
          "misses": s.misses,
          "size": s.size,
        }))
      })
    })
  }

  /// Bounds the check by `timeout` (default [`DEFAULT_CHECK_TIMEOUT`]).
  pub fn with_timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// The indicator's name (its key in [`HealthReport::checks`]).
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The check's timeout.
  pub fn timeout(&self) -> Duration {
    self.timeout
  }

  /// The check as an effect that never fails: it runs under [`Effect::timeout`] and reports a
  /// failure, defect or timeout as `Down`.
  pub fn run(&self) -> Effect<CheckReport, Never> {
    let check = Arc::clone(&self.check);
    let timeout = self.timeout;
    sync(Instant::now).flat_map(move |start: Instant| {
      suspend(move || check())
        .exit::<Never>()
        .timeout(timeout)
        .map(move |out| {
          let outcome = match out {
            Some(Exit::Success(outcome)) => outcome,
            Some(Exit::Failure(cause)) => {
              HealthOutcome::down(json!({ "error": failure_message(cause) }))
            }
            None => HealthOutcome::down(json!({
              "error": "timed out",
              "timeout_ms": millis(timeout),
            })),
          };
          CheckReport {
            status: outcome.status,
            detail: outcome.detail,
            duration_ms: millis(start.elapsed()),
          }
        })
    })
  }
}

/// The aggregated readiness answer: the worst check status (forced `Down` when the registry's
/// [`ReadinessState`] is not ready, with its `reason`) and every check by name.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HealthReport {
  /// Overall verdict.
  pub status: HealthStatus,
  /// Why the [`ReadinessState`] is not ready (`shutting_down`, `not_ready`), when it is not.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub reason: Option<String>,
  /// Each check's report, by indicator name.
  pub checks: BTreeMap<String, CheckReport>,
}

impl HealthReport {
  /// `503` when [`status`](Self::status) is `Down`, otherwise `200`.
  pub fn http_status(&self) -> StatusCode {
    if self.status == HealthStatus::Down {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      StatusCode::OK
    }
  }
}

impl IntoResponse for HealthReport {
  fn into_response(self) -> Response {
    (self.http_status(), Json(self)).into_response()
  }
}

/// A set of [`HealthIndicator`]s (and optionally a [`ReadinessState`]) behind `/ready`.
#[derive(Clone, Debug, Default)]
pub struct HealthRegistry {
  indicators: Vec<HealthIndicator>,
  readiness: Option<ReadinessState>,
}

impl HealthRegistry {
  /// An empty registry (always `Up`).
  pub fn new() -> Self {
    Self::default()
  }

  /// Adds `indicator` (a later indicator with the same name replaces it in the report).
  pub fn with_indicator(mut self, indicator: HealthIndicator) -> Self {
    self.indicators.push(indicator);
    self
  }

  /// Gates the report on `state`: while it is not ready the report is `Down` with its
  /// [`not_ready_reason`](ReadinessState::not_ready_reason). Pass the host's
  /// [`Host::readiness`](crate::Host::readiness) so `/ready` turns `503` on shutdown.
  pub fn with_readiness(mut self, state: ReadinessState) -> Self {
    self.readiness = Some(state);
    self
  }

  /// The registered indicators.
  pub fn indicators(&self) -> &[HealthIndicator] {
    &self.indicators
  }

  /// Runs every check concurrently (each under its own timeout) and aggregates the report.
  pub fn check(&self) -> Effect<HealthReport, Never> {
    let names: Vec<String> = self.indicators.iter().map(|i| i.name.clone()).collect();
    let runs = self.indicators.iter().map(HealthIndicator::run).collect();
    let readiness = self.readiness.clone();
    fork_all(runs, Concurrency::Unbounded).map(move |reports: Vec<CheckReport>| {
      let reason = readiness
        .as_ref()
        .and_then(ReadinessState::not_ready_reason);
      let checked = reports
        .iter()
        .fold(HealthStatus::Up, |acc, r| acc.worst(r.status));
      HealthReport {
        status: if reason.is_some() {
          HealthStatus::Down
        } else {
          checked
        },
        reason: reason.map(str::to_owned),
        checks: names.into_iter().zip(reports).collect(),
      }
    })
  }

  /// `GET` readiness route answering the JSON [`HealthReport`] (`200` or `503`).
  pub fn ready_route(&self) -> MethodRouter {
    let registry = self.clone();
    get(move || {
      let registry = registry.clone();
      async move {
        match crate::EffectExecutor::global()
          .run_effect(registry.check())
          .await
        {
          Exit::Success(report) => report.into_response(),
          Exit::Failure(cause) => crate::cause_response(&cause),
        }
      }
    })
  }

  /// Router with `/health` (liveness) and `/ready` ([`ready_route`](Self::ready_route)).
  pub fn routes(&self) -> Router {
    Router::new()
      .route("/health", health())
      .route("/ready", self.ready_route())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use tower::ServiceExt;

  #[tokio::test]
  async fn health_returns_ok() {
    let app = health_router();
    let res = app
      .oneshot(
        Request::builder()
          .uri("/health")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn ready_returns_service_unavailable_when_check_fails() {
    let check: ReadinessCheck = Arc::new(|| false);
    let app = observability_routes(check);
    let res = app
      .oneshot(
        Request::builder()
          .uri("/ready")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
  }

  #[tokio::test]
  async fn ready_state_flips_with_set_ready() {
    let state = ReadinessState::new(false);
    let app = observability_routes_with_state(state.clone());
    let req = Request::builder()
      .uri("/ready")
      .body(Body::empty())
      .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    state.set_ready(true);
    let req = Request::builder()
      .uri("/ready")
      .body(Body::empty())
      .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn require_ready_middleware_blocks_app_routes() {
    let state = ReadinessState::new(false);
    let app = Router::new()
      .route("/work", get(|| async { "done" }))
      .route("/health", health())
      .route("/ready", ready_with_state(state.clone()))
      .layer(axum::middleware::from_fn_with_state(
        state.clone(),
        require_ready,
      ));

    let req = Request::builder().uri("/work").body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);

    state.set_ready(true);
    let req = Request::builder().uri("/work").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
  }
}
