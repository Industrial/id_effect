//! 091 — a server-sent events ticker that resumes from `Last-Event-ID` (feature `sse`).
//!
//! Every event carries its sequence number as id; a reconnecting `EventSource` sends the last id
//! it saw, and the stream continues right after it. A heartbeat comment goes out every 15 s.
//!
//! ```text
//! cargo run -p id_effect_axum --features sse --example 091_sse_ticker               # serve on :3001
//! cargo run -p id_effect_axum --features sse --example 091_sse_ticker -- --self-test
//! curl -N -H 'Last-Event-ID: 41' http://127.0.0.1:3001/ticks
//! ```

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::{Effect, Stream, sleep, succeed};
use id_effect_axum::RequestCaps;
use id_effect_axum::realtime::sse::{self, SseConfig, SseEvent, SseRequest};
use id_effect_axum::route::Router;
use id_effect_axum::{ConnectionLimits, serve_listener};
use tower::ServiceExt;

/// One tick per `period`, numbered from just after the client's last event id; `limit` ticks.
fn ticks(
  period: Duration,
  limit: u64,
) -> impl Fn(&mut (), RequestCaps, SseRequest) -> Effect<Stream<SseEvent>, Infallible, ()> + Clone {
  move |_, _caps, req| {
    let from = req
      .last_event_id
      .and_then(|id| id.as_u64())
      .map_or(0, |n| n + 1);
    // One event per pull, so each tick goes out as soon as it is due.
    let mut next = from;
    let events = Stream::from_pull(move |_max| {
      let n = next;
      next += 1;
      if n >= from.saturating_add(limit) {
        return succeed(None);
      }
      sleep(period).map(move |()| {
        let event = SseEvent::data(format!("tick {n}"))
          .event("tick")
          .id(n.to_string());
        Some(vec![event])
      })
    });
    succeed(events)
  }
}

fn app(period: Duration, limit: u64) -> axum::Router {
  let config = SseConfig::default().retry(Duration::from_secs(2));
  Router::new()
    .route(sse::endpoint("/ticks", config, ticks(period, limit)).public())
    .with_state(())
    .into_axum()
}

async fn self_test() {
  let app = app(Duration::from_millis(1), 3);
  let req = Request::get("/ticks")
    .header("last-event-id", "41")
    .body(Body::empty())
    .unwrap();
  let res = app.oneshot(req).await.unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  let body = res.into_body().collect().await.unwrap().to_bytes();
  let body = String::from_utf8(body.to_vec()).unwrap();
  let ids: Vec<&str> = body
    .lines()
    .filter_map(|l| l.strip_prefix("id: "))
    .collect();
  assert_eq!(ids, ["42", "43", "44"], "{body}");
  assert!(body.starts_with("retry: 2000\n\n"));
  println!("091_sse_ticker self-test: ok (resumed after Last-Event-ID 41)");
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
  if std::env::args().any(|a| a == "--self-test") {
    self_test().await;
    return Ok(());
  }
  let listener = tokio::net::TcpListener::bind("127.0.0.1:3001").await?;
  println!("http://{}/ticks", listener.local_addr()?);
  serve_listener(
    listener,
    app(Duration::from_secs(1), u64::MAX / 2),
    ConnectionLimits::new(),
    std::future::pending(),
  )
  .await
}
