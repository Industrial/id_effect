//! Aggregated readiness: a [`HealthRegistry`] of effect-backed [`HealthIndicator`]s behind
//! `/ready`. Checks run concurrently under per-check timeouts; a hung check reports `down`, and
//! marking the readiness shutting down (what the host does on SIGTERM) turns `/ready` into `503`.
//!
//! Run: `cargo run -p id_effect_axum --example 071_health`

use std::convert::Infallible;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use id_effect::{Cache, sleep, succeed};
use id_effect_axum::{HealthIndicator, HealthOutcome, HealthRegistry, ReadinessState};
use serde_json::json;
use tower::ServiceExt;

async fn ready(app: &Router) -> String {
  let req = Request::builder()
    .uri("/ready")
    .body(Body::empty())
    .unwrap();
  let res = app.clone().oneshot(req).await.unwrap();
  let status = res.status();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  format!("{status} {}", String::from_utf8_lossy(&bytes))
}

fn registry(readiness: &ReadinessState, hung: bool) -> HealthRegistry {
  let users: Cache<u32, String> = Cache::new(1_000, None, |id: u32| succeed(format!("user-{id}")));
  let mut registry = HealthRegistry::new()
    .with_readiness(readiness.clone())
    // Any effect: success is `up` with its value as detail.
    .with_indicator(HealthIndicator::from_effect("db", || {
      sleep::<Infallible, _>(Duration::from_millis(20)).map(|()| json!({ "pool": "ok" }))
    }))
    // Full control over the verdict.
    .with_indicator(HealthIndicator::new("queue", || {
      succeed::<_, Infallible, _>(HealthOutcome::degraded(json!({ "lag": 42 })))
    }))
    .with_indicator(HealthIndicator::cache("user_cache", users));
  if hung {
    registry = registry.with_indicator(
      HealthIndicator::new("broker", || {
        sleep::<Infallible, _>(Duration::from_secs(3600)).map(|()| HealthOutcome::up())
      })
      .with_timeout(Duration::from_millis(50)),
    );
  }
  registry
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let readiness = ReadinessState::new(true);

  let app = registry(&readiness, false).routes();
  println!("healthy:       {}", ready(&app).await);

  let app = registry(&readiness, true).routes();
  println!("hung check:    {}", ready(&app).await);

  // The host's shutdown paths do this before draining in-flight requests.
  readiness.mark_shutting_down();
  let app = registry(&readiness, false).routes();
  println!("shutting down: {}", ready(&app).await);
}
