//! Application host composition: config bootstrap, security middleware, platform auth traits.

use std::sync::Arc;

use axum::{Router, body::Body, http::Request, middleware};
use id_effect::{Effect, run_blocking, succeed};
use id_effect_axum::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, HostBuilder, bootstrap_env, csp_middleware,
  csrf_middleware, routing,
};
use id_effect_config::MapConfigProvider;
use id_effect_platform::auth::{MemorySessionStore, SessionData, SessionStore};
use tower::ServiceExt;

#[derive(Clone)]
struct AppState {
  sessions: MemorySessionStore,
}

fn session_user(env: &mut AppState) -> Effect<String, std::convert::Infallible, AppState> {
  let store = env.sessions.clone();
  succeed(match run_blocking(store.get("demo"), ()) {
    Ok(Some(data)) => format!("user={}", data.user_id),
    _ => "user=none".into(),
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

  let state = AppState {
    sessions: MemorySessionStore::default(),
  };
  run_blocking(state.sessions.put("demo", SessionData::new("user-1")), ()).expect("seed session");

  let csp = Arc::new(ContentSecurityPolicy::new().default_src_self());
  let csrf = CsrfConfig::new("demo-token");

  let app = Router::new()
    .route("/", routing::get(session_user))
    .layer(middleware::from_fn_with_state(csp, csp_middleware))
    .layer(middleware::from_fn_with_state(csrf, csrf_middleware))
    .with_state(state);

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
