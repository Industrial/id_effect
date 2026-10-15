//! An asynchronous handler effect: the kernel's `sleep` (executor clock) and a `from_async` step
//! awaiting tokio, run as the request's fiber.
//!
//! Run: `cargo run -p id_effect_axum --example 020_async_effect`

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::Router;
use http_body_util::BodyExt;
use id_effect::next::{Effect, from_async, sleep};
use id_effect_axum::routing;
use std::convert::Infallible;
use std::time::Duration;
use tower::ServiceExt;

#[derive(Clone)]
struct AppState;

fn delayed_ok(_env: &mut AppState) -> Effect<&'static str, Infallible, AppState> {
  sleep(Duration::from_millis(1)).then(from_async(|| async {
    tokio::time::sleep(Duration::from_millis(1)).await;
    Ok("async step finished")
  }))
}

#[tokio::main]
async fn main() {
  let app = Router::new()
    .route("/wait", routing::get(delayed_ok))
    .with_state(AppState);

  let res = app
    .oneshot(Request::builder().uri("/wait").body(Body::empty()).unwrap())
    .await
    .unwrap();

  assert_eq!(res.status(), StatusCode::OK);
  let body = res.into_body().collect().await.unwrap().to_bytes();
  assert_eq!(body.as_ref(), b"async step finished");
  println!("020_async_effect ok");
}
