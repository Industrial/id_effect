//! Liveness (`/health`) and readiness (`/ready`) route helpers for Axum services.

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use std::sync::Arc;

/// Async readiness predicate; return `true` when the service can accept traffic.
pub type ReadinessCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Shared flip-switch for readiness (for example set `false` during drain, `true` after startup).
#[derive(Clone, Default)]
pub struct ReadinessState {
  ready: Arc<std::sync::atomic::AtomicBool>,
}

impl ReadinessState {
  /// Creates state with an initial readiness flag.
  pub fn new(initial: bool) -> Self {
    Self {
      ready: Arc::new(std::sync::atomic::AtomicBool::new(initial)),
    }
  }

  /// Updates whether the service accepts traffic.
  pub fn set_ready(&self, ready: bool) {
    self.ready.store(ready, std::sync::atomic::Ordering::SeqCst);
  }

  /// Current readiness flag.
  pub fn is_ready(&self) -> bool {
    self.ready.load(std::sync::atomic::Ordering::SeqCst)
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
