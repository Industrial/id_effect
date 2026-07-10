//! Axum + capability DI: `build_env` → `State<Env>` → `run_with_caps` → handler effect.

use axum::{Router, body::Body, extract::State, http::Request, routing::get};
use id_effect::{Effect, Env, Needs, build_env, effect, provide};
use id_effect_axum::run_with_caps;
use tower::ServiceExt;

#[derive(Clone, Copy, Debug)]
struct Counter(u32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Counter)]
struct CounterLive;

impl CounterLive {
  #[allow(clippy::new_ret_no_self)]
  fn new() -> Counter {
    Counter(7)
  }
}

fn handler(_env: &mut Env) -> Effect<String, (), Env> {
  effect!(|r: &mut Env| { format!("count={}", Needs::<Counter>::need(r).0) })
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
  let env = build_env([provide!(CounterLive)]).expect("env");
  let app = Router::new()
    .route(
      "/",
      get(
        |State(env): State<Env>| async move { run_with_caps(State(env), handler).await.unwrap() },
      ),
    )
    .with_state(env);

  let res = app
    .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
    .await
    .unwrap();
  let body = http_body_util::BodyExt::collect(res.into_body())
    .await
    .unwrap()
    .to_bytes();
  assert_eq!(&body[..], b"count=7");
  println!("020_capability_run_with ok");
}
