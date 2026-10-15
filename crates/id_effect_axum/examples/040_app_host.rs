//! Application host composition: config bootstrap, security middleware, platform auth traits.
//!
//! Run: `cargo run -p id_effect_axum --example 040_app_host`

use std::convert::Infallible;
use std::sync::Arc;

use axum::{Router, body::Body, http::Request, middleware};
use id_effect::Exit;
use id_effect::{Effect, HCons, HNil, hlist, need};
use id_effect_axum::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, EffectExecutor, HostBuilder, bootstrap_env,
  csp_middleware, csrf_middleware, routing,
};
use id_effect_config::MapConfigProvider;
use id_effect_platform::auth::{MemorySessionStore, SessionData, SessionStore};
use tower::ServiceExt;

type AppEnv = HCons<MemorySessionStore, HNil>;

fn session_user(_env: &mut AppEnv) -> Effect<String, Infallible, AppEnv> {
  need::<MemorySessionStore>().flat_map(|store| {
    store.get("demo").exit().narrow().map(|exit| match exit {
      Exit::Success(Some(data)) => format!("user={}", data.user_id),
      _ => "user=none".into(),
    })
  })
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
  let provider = MapConfigProvider::from_pairs([("PORT", "8080")]);
  let env = bootstrap_env(provider);
  let host = HostBuilder::new()
    .with_env(env)
    .build()
    .await
    .expect("host");
  assert_eq!(host.config.bind_port, 8080);

  let sessions = MemorySessionStore::default();
  let seeded = EffectExecutor::global()
    .run_effect(sessions.put("demo", SessionData::new("user-1")))
    .await;
  assert!(matches!(seeded, Exit::Success(())), "seed session");

  let csp = Arc::new(ContentSecurityPolicy::new().default_src_self());
  let csrf = CsrfConfig::new("demo-token");

  let app = Router::new()
    .route("/", routing::get(session_user))
    .layer(middleware::from_fn_with_state(csp, csp_middleware))
    .layer(middleware::from_fn_with_state(csrf, csrf_middleware))
    .with_state(hlist![sessions]);

  let res = app
    .oneshot(
      Request::builder()
        .uri("/")
        .header(CSRF_HEADER, "demo-token")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  let body = http_body_util::BodyExt::collect(res.into_body())
    .await
    .unwrap()
    .to_bytes();
  assert_eq!(&body[..], b"user=user-1");
  println!("040_app_host ok");
}
