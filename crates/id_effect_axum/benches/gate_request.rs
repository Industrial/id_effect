//! D9 gate benches for the axum request path, driven in-process through `oneshot` (no network).
//!
//! - `gate/axum_request_oneshot`: a [`routing::get`] Effect handler, request in, body collected.
//! - `gate/axum_request_caps`: the same round trip through [`CapOverrideLayer`] and
//!   [`execute_with_request_caps`], so the per-request capability overlay is on the path.
//!
//! Since 0.7 each request's effect runs as a fiber on the serving tokio runtime (0.6: a hop to a
//! dedicated executor thread); the bench IDs are unchanged.
//!
//! Run: `cargo bench -p id_effect_axum --bench gate_request`

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::BodyExt;
use id_effect::{DynEnv, Effect, succeed};
use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps, routing};
use std::convert::Infallible;
use std::hint::black_box;
use tower::ServiceExt;

#[derive(Clone)]
struct AppState;

struct Tenant(String);

fn hello(_env: &mut AppState) -> Effect<&'static str, Infallible, AppState> {
  succeed("hello")
}

fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  let tenant = env
    .try_get::<Tenant>()
    .map_or_else(|_| "none".to_owned(), |t| t.0.clone());
  succeed(format!("tenant={tenant}"))
}

fn runtime() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime")
}

async fn round_trip(app: &Router, req: Request<Body>) -> usize {
  let res = app.clone().oneshot(req).await.expect("infallible router");
  assert_eq!(res.status(), StatusCode::OK);
  let body = res.into_body().collect().await.expect("body").to_bytes();
  body.len()
}

fn bench_axum_request(c: &mut Criterion) {
  let rt = runtime();
  let mut group = c.benchmark_group("gate");

  let plain = Router::new()
    .route("/hello", routing::get(hello))
    .with_state(AppState);
  group.bench_function("axum_request_oneshot", |b| {
    b.iter(|| {
      let req = Request::builder()
        .uri("/hello")
        .body(Body::empty())
        .expect("request");
      black_box(rt.block_on(round_trip(&plain, req)))
    });
  });

  let base = DynEnv::new().with(Tenant("public".into()));
  let caps = Router::new()
    .route(
      "/whoami",
      axum::routing::get(|st: State<DynEnv>, caps: RequestCaps| {
        execute_with_request_caps(st, caps, whoami)
      }),
    )
    .layer(CapOverrideLayer::new(|parts| {
      let tenant = parts.headers.get("x-tenant")?.to_str().ok()?;
      Some(Tenant(tenant.to_owned()))
    }))
    .with_state(base);
  group.bench_function("axum_request_caps", |b| {
    b.iter(|| {
      let req = Request::builder()
        .uri("/whoami")
        .header("x-tenant", "acme")
        .body(Body::empty())
        .expect("request");
      black_box(rt.block_on(round_trip(&caps, req)))
    });
  });

  group.finish();
}

criterion_group!(benches, bench_axum_request);
criterion_main!(benches);
