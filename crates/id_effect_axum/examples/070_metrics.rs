//! Prometheus metrics: [`http_metrics_layer`] records every request by method, route template
//! and status; [`metrics_router`] serves `GET /metrics` (registry + runtime gauges).
//!
//! Run: `cargo run -p id_effect_axum --example 070_metrics`

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::Router;
use http_body_util::BodyExt;
use id_effect::{Counter, Effect, succeed};
use id_effect_axum::metrics::{http_metrics_layer, metrics_router};
use id_effect_axum::routing;
use std::convert::Infallible;
use tower::ServiceExt;

#[derive(Clone)]
struct AppState {
  greetings: Counter,
}

fn hello(env: &mut AppState) -> Effect<String, Infallible, AppState> {
  env
    .greetings
    .increment()
    .map(|()| "hello from effect-axum".to_string())
}

fn user(_: &mut AppState) -> Effect<&'static str, Infallible, AppState> {
  succeed("a user")
}

#[tokio::main]
async fn main() {
  let app = Router::new()
    .route("/", routing::get(hello))
    .route("/users/{id}", routing::get(user))
    .merge(metrics_router())
    .layer(http_metrics_layer())
    .with_state(AppState {
      greetings: Counter::new("greetings"),
    });

  for uri in ["/", "/users/1", "/users/2", "/missing"] {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    app.clone().oneshot(req).await.unwrap();
  }

  let req = Request::builder()
    .uri("/metrics")
    .body(Body::empty())
    .unwrap();
  let res = app.oneshot(req).await.unwrap();
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(
    res.headers()[header::CONTENT_TYPE],
    "text/plain; version=0.0.4; charset=utf-8"
  );
  let body = res.into_body().collect().await.unwrap().to_bytes();
  let body = String::from_utf8(body.to_vec()).unwrap();
  print!("{body}");
  assert!(body.contains("greetings_total 1\n"));
  assert!(body.contains(
    "http_server_requests_total{method=\"GET\",route=\"/users/{id}\",status=\"200\"} 2\n"
  ));
  assert!(
    body.contains(
      "http_server_requests_total{method=\"GET\",route=\"unmatched\",status=\"404\"} 1\n"
    )
  );
  assert!(body.contains("# TYPE id_effect_fibers_live gauge\n"));
  println!("070_metrics ok");
}
