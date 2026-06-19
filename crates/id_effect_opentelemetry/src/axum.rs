//! Axum middleware: W3C trace context extraction and server spans.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::Context;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::propagation::extract_trace_context_from_headers;

/// Create a server span from incoming W3C headers and run the rest of the stack under it.
pub async fn trace_request(request: Request, next: Next) -> Response {
  let pairs: Vec<(String, String)> = request
    .headers()
    .iter()
    .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
    .collect();
  let parent = extract_trace_context_from_headers(&Context::new(), &pairs);
  let span = tracing::info_span!("http.request", otel.name = %request.uri().path());
  let _ = span.set_parent(parent);
  let _guard = span.enter();
  next.run(request).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::body::Body;
  use axum::routing::get;
  use tower::ServiceExt;

  #[tokio::test]
  async fn trace_request_middleware_runs() {
    let app = Router::new()
      .route("/", get(|| async { "ok" }))
      .layer(axum::middleware::from_fn(trace_request));
    let res = app
      .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::OK);
  }
}
