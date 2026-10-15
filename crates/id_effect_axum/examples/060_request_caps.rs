//! Request-scoped capability overrides: [`CapOverrideLayer`] reads `x-tenant` per request and the
//! effect sees that tenant instead of the router's base one — without touching shared state.
//!
//! Run: `cargo run -p id_effect_axum --example 060_request_caps`

use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::routing::{Router, get};
use http_body_util::BodyExt;
use id_effect::next::{DynEnv, Effect, succeed};
use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps};
use std::convert::Infallible;
use tower::ServiceExt;

struct Tenant(String);

fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  let tenant = env
    .try_get::<Tenant>()
    .map_or_else(|_| "none".to_owned(), |t| t.0.clone());
  succeed(format!("tenant={tenant}"))
}

async fn get_body(app: &Router, req: Request<Body>) -> String {
  let res = app.clone().oneshot(req).await.unwrap();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
  let base = DynEnv::new().with(Tenant("public".into()));

  let app = Router::new()
    .route(
      "/whoami",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
    )
    .layer(CapOverrideLayer::new(|parts| {
      let tenant = parts.headers.get("x-tenant")?.to_str().ok()?;
      Some(Tenant(tenant.to_owned()))
    }))
    .with_state(base);

  let acme = Request::builder()
    .uri("/whoami")
    .header("x-tenant", "acme")
    .body(Body::empty())
    .unwrap();
  let plain = Request::builder()
    .uri("/whoami")
    .body(Body::empty())
    .unwrap();

  let a = get_body(&app, acme).await;
  let b = get_body(&app, plain).await;
  assert_eq!(a, "tenant=acme");
  assert_eq!(b, "tenant=public");
  println!("060_request_caps: ok ({a}, {b})");
}
