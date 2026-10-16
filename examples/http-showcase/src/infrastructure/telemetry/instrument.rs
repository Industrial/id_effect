//! [`instrument`]: the showcase's routes wrapped in the process-wide request telemetry.

use axum::Router;

use super::Telemetry;

/// [`Telemetry::global`]`().`[`instrument`](Telemetry::instrument)`(router)`: the wiring the
/// showcase applies around its routes (one span and one latency observation per request,
/// exported when `OTEL_EXPORTER_OTLP_ENDPOINT` is set).
pub fn instrument(router: Router) -> Router {
  Telemetry::global().instrument(router)
}

#[cfg(test)]
mod tests {
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use axum::routing::get;
  use tower::ServiceExt as _;

  use super::*;

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn the_global_wiring_passes_responses_through() {
    let app = instrument(Router::new().route("/ping", get(|| async { "pong" })));
    let request = Request::builder().uri("/ping").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
  }
}
