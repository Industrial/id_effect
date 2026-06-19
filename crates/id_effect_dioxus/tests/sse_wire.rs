//! SSE handler smoke test.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::{Router, routing::get};
use id_effect_dioxus::{RealtimeEvent, RealtimeHub, sse_handler};
use tower::ServiceExt;

#[tokio::test]
async fn sse_handler_returns_event_stream() {
  let hub = Arc::new(RealtimeHub::new(8));
  let app = Router::new().route(
    "/events",
    get({
      let hub = Arc::clone(&hub);
      move || async move { sse_handler(hub) }
    }),
  );
  let res = app
    .oneshot(
      Request::builder()
        .uri("/events")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  let ct = res
    .headers()
    .get(header::CONTENT_TYPE)
    .unwrap()
    .to_str()
    .unwrap();
  assert!(ct.contains("text/event-stream"));
  hub.publish(RealtimeEvent {
    topic: "t".into(),
    event: "ping".into(),
    data_json: "{}".into(),
  });
}
