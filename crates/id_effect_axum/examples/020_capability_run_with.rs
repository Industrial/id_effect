//! Axum + capabilities: a typed capability list as router state, read with `need`; and a
//! runtime-checked [`DynEnv`] through [`run_with_caps`].
//!
//! Run: `cargo run -p id_effect_axum --example 020_capability_run_with`

use axum::{Router, body::Body, extract::State, http::Request, routing::get};
use id_effect::Exit;
use id_effect::{DynEnv, Effect, HCons, HNil, hlist, need, succeed};
use id_effect_axum::{routing, run_with_caps};
use std::convert::Infallible;
use tower::ServiceExt;

#[derive(Clone, Copy, Debug)]
struct Counter(u32);

type AppEnv = HCons<Counter, HNil>;

fn typed(_env: &mut AppEnv) -> Effect<String, Infallible, AppEnv> {
  need::<Counter>().map(|c| format!("count={}", c.0))
}

fn dynamic(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  succeed(format!(
    "count={}",
    env.try_get::<Counter>().map_or(0, |c| c.0)
  ))
}

async fn body(app: Router, uri: &str) -> String {
  let res = app
    .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
    .await
    .unwrap();
  let bytes = http_body_util::BodyExt::collect(res.into_body())
    .await
    .unwrap()
    .to_bytes();
  String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
  let typed_app = Router::new()
    .route("/", routing::get(typed))
    .with_state(hlist![Counter(7)]);
  assert_eq!(body(typed_app, "/").await, "count=7");

  let dyn_app = Router::new()
    .route(
      "/",
      get(|State(env): State<DynEnv>| async move {
        match run_with_caps(State(env), dynamic).await {
          Exit::Success(s) => s,
          Exit::Failure(cause) => format!("failed: {cause:?}"),
        }
      }),
    )
    .with_state(DynEnv::new().with(Counter(9)));
  assert_eq!(body(dyn_app, "/").await, "count=9");
  println!("020_capability_run_with ok");
}
