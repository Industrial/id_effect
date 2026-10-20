//! D9 gate benches for the axum request path, driven in-process through `oneshot` (no network).
//!
//! - `gate/axum_request_oneshot`: a [`routing::get`] Effect handler, request in, body collected.
//! - `gate/axum_request_caps`: the same round trip through [`CapOverrideLayer`] and
//!   [`execute_with_request_caps`], so the per-request capability overlay is on the path.
//! - `gate/axum_request_bare` and `gate/axum_request_secure_stack` (0.11, E2.6): the pair behind
//!   the secure-stack KPI, a `kind = "delta"` gate `secure_stack − bare ≤ 5000 ns` in
//!   `docs/benchmarks/gates.toml`. Both send the same request (peer address, API key) to an
//!   effect handler answering `hello`; `secure_stack` puts the recommended production stack in
//!   front of it:
//!   [`RateLimitLayer`] (per client IP), [`LoadShedLayer`] (in-flight cap),
//!   [`SecurityLayer::recommended`] (CSRF, CSP nonce, security headers, cookie defaults),
//!   [`Authenticate`] (API key) and a route [`Guard`] (`authenticated()`), and the handler writes
//!   one access line through the logger's default [`RedactionPolicy`]. The stack grows as the
//!   0.11 security layers land; `r11-w3-kpis` re-measures it with the final one.
//!
//! Since 0.7 each request's effect runs as a fiber on the serving tokio runtime (0.6: a hop to a
//! dedicated executor thread); the bench IDs are unchanged.
//!
//! Run: `cargo bench -p id_effect_axum --bench gate_request`
//!
//! [`RedactionPolicy`]: id_effect_logger::RedactionPolicy

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode};
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::BodyExt;
use id_effect::{DynEnv, Effect, HNil, succeed};
use id_effect_auth::{ApiKey, ApiKeys, AuthPrincipal, Authenticate, MemoryApiKeyStore};
use id_effect_authz::{AuthorizeExt, Guard, authenticated};
use id_effect_axum::middleware::LoadShedLayer;
use id_effect_axum::{
  CapOverrideLayer, CookieKey, RateLimitLayer, RequestCaps, SecurityLayer,
  execute_with_request_caps, routing,
};
use id_effect_logger::EffectLogger;
use std::convert::Infallible;
use std::hint::black_box;
use std::net::SocketAddr;
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

/// The access line the secure stack's handler writes; the default redaction policy scrubs it.
const ACCESS_LINE: &str = "access method=GET path=/pair status=200 api_key=bench_0123456789abcdef";

fn pair_hello(_env: &mut HNil) -> Effect<&'static str, Infallible, HNil> {
  succeed("hello")
}

fn logged_hello(_env: &mut HNil) -> Effect<&'static str, StatusCode, HNil> {
  EffectLogger
    .info(ACCESS_LINE)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    .map(|()| "hello")
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

  // The secure-stack pair: the same request (peer address, API key) and handler, bare and behind
  // the stack. Each layer must let it through, so `round_trip`'s `200` check proves the stack ran.
  let (key, record) = ApiKey::generate("bench", AuthPrincipal::new("svc-bench")).expect("api key");
  let keys = MemoryApiKeyStore::new();
  keys.insert(record);
  let pair_request = || {
    Request::builder()
      .uri("/pair")
      .header("x-api-key", key.expose())
      .extension(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
      .body(Body::empty())
      .expect("request")
  };

  let bare = Router::new()
    .route("/pair", routing::get(pair_hello))
    .with_state(HNil);
  group.bench_function("axum_request_bare", |b| {
    b.iter(|| black_box(rt.block_on(round_trip(&bare, pair_request()))));
  });

  // `Router::layer` wraps in call order, the last outermost: rate limit, load shedding, security
  // layer, authentication, then the route's guard.
  let secure = Router::new()
    .route(
      "/pair",
      routing::get(logged_hello).authorize(Guard::new("pair", authenticated())),
    )
    .layer(Authenticate::api_key(ApiKeys::new(keys, "bench")))
    .layer(SecurityLayer::recommended(CookieKey::generate()))
    .layer(LoadShedLayer::new(1024))
    .layer(RateLimitLayer::per_second(1e9, 1e9))
    .with_state(HNil);
  group.bench_function("axum_request_secure_stack", |b| {
    b.iter(|| black_box(rt.block_on(round_trip(&secure, pair_request()))));
  });

  group.finish();
}

criterion_group!(benches, bench_axum_request);
criterion_main!(benches);
