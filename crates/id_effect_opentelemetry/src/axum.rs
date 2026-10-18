//! Axum middleware: W3C trace context extraction and server spans.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::Context;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::propagation::extract_trace_context_from_headers;

/// Create a server span from incoming W3C headers and run the rest of the stack under it.
///
/// The span is entered only while the rest of the stack is polled
/// ([`Instrument`](tracing::Instrument)), never across an `.await`, so other tasks polled on the
/// same thread while the handler waits do not run inside it.
pub async fn trace_request(request: Request, next: Next) -> Response {
  let pairs: Vec<(String, String)> = request
    .headers()
    .iter()
    .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
    .collect();
  let parent = extract_trace_context_from_headers(&Context::new(), &pairs);
  let span = tracing::info_span!("http.request", otel.name = %request.uri().path());
  let _ = span.set_parent(parent);
  next.run(request).instrument(span).await
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

  /// The name of the thread's current `tracing` span.
  fn current_span_name() -> Option<&'static str> {
    tracing::Span::current().metadata().map(|m| m.name())
  }

  #[tokio::test]
  async fn server_span_not_held_across_await() {
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::{TraceContextExt, Tracer, TracerProvider as _};
    use opentelemetry_sdk::trace::InMemorySpanExporter;

    use crate::propagation::{inject_trace_context_into_headers, install_w3c_propagators};
    use crate::subscriber::{
      sdk_tracer_provider_with_in_memory_exporter, trace_subscriber_for_provider,
    };

    install_w3c_propagators();
    let exporter = InMemorySpanExporter::default();
    let provider = sdk_tracer_provider_with_in_memory_exporter(&exporter);
    let _subscriber =
      tracing::subscriber::set_default(trace_subscriber_for_provider(&provider, false, None));
    let remote = Context::new().with_span(provider.tracer("remote").start("remote"));
    let mut headers = Vec::new();
    inject_trace_context_into_headers(&remote, &mut headers);

    // The handler suspends a few times; between its polls the same task polls `observer` on
    // the same thread (current-thread runtime), which must not see the server span.
    let inside = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&inside);
    let app = Router::new()
      .route(
        "/items",
        get(move || async move {
          for _ in 0..4 {
            seen.lock().unwrap().push(current_span_name());
            tokio::task::yield_now().await;
          }
          "ok"
        }),
      )
      .layer(axum::middleware::from_fn(trace_request));
    let mut builder = Request::builder().uri("/items");
    for (k, v) in &headers {
      builder = builder.header(k.as_str(), v.as_str());
    }
    let observer = async {
      let mut outside = Vec::new();
      for _ in 0..8 {
        outside.push(current_span_name());
        tokio::task::yield_now().await;
      }
      outside
    };
    let (res, outside) = tokio::join!(app.oneshot(builder.body(Body::empty()).unwrap()), observer);
    assert_eq!(res.unwrap().status(), axum::http::StatusCode::OK);

    assert!(
      inside
        .lock()
        .unwrap()
        .iter()
        .all(|n| *n == Some("http.request")),
      "the handler runs in the server span"
    );
    assert!(
      outside.iter().all(|n| *n != Some("http.request")),
      "the server span leaked to a sibling future: {outside:?}"
    );
    let spans = exporter.get_finished_spans().expect("spans");
    let server = spans
      .iter()
      .find(|s| s.name == "/items")
      .expect("server span exported");
    assert_eq!(
      server.span_context.trace_id(),
      remote.span().span_context().trace_id()
    );
    assert_eq!(
      server.parent_span_id,
      remote.span().span_context().span_id()
    );
    let _ = provider.shutdown();
  }
}
