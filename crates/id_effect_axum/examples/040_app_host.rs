//! Application host composition: config bootstrap, the secure-by-default `SecurityLayer`
//! (CSRF, CSP nonce, security headers, cookie defaults), CORS, platform auth traits.
//!
//! Run: `cargo run -p id_effect_axum --example 040_app_host`

use std::convert::Infallible;
use std::sync::Arc;

use axum::http::{Method, StatusCode, header};
use axum::response::Response;
use axum::{Router, body::Body, http::Request};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use id_effect::Exit;
use id_effect::{Effect, HCons, HNil, hlist, need};
use id_effect_axum::server::security::COOKIE_KEY_ENV;
use id_effect_axum::{
  CSRF_HEADER, CookieKey, CorsConfig, CsrfToken, EffectExecutor, HostBuilder, SecurityLayer,
  bootstrap_env, routing,
};
use id_effect_config::{ConfigError, ConfigProvider, MapConfigProvider};
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

async fn send(app: &Router, request: Request<Body>) -> Response {
  app.clone().oneshot(request).await.expect("infallible")
}

async fn body_text(response: Response) -> String {
  let bytes = http_body_util::BodyExt::collect(response.into_body())
    .await
    .expect("body")
    .to_bytes();
  String::from_utf8(bytes.to_vec()).expect("utf-8")
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

  // The cookie key arrives through an effect layer (here from config; `COOKIE_KEY` is base64 of
  // at least 64 random bytes — never hard-code it outside a demo).
  let demo_key = STANDARD.encode([7u8; 64]);
  let key_config = MapConfigProvider::from_pairs([(COOKIE_KEY_ENV, demo_key.as_str())]);
  let key_layer = key_config
    .layer()
    .provide_to(CookieKey::layer::<ConfigError>());
  let load_key: Effect<CookieKey, ConfigError, HCons<CookieKey, HNil>> =
    need::<CookieKey>().map(|key: Arc<CookieKey>| (*key).clone());
  let Exit::Success(key) = EffectExecutor::global()
    .run_effect(load_key.provide_layer(key_layer))
    .await
  else {
    panic!("cookie key");
  };

  let sessions = MemorySessionStore::default();
  let seeded = EffectExecutor::global()
    .run_effect(sessions.put("demo", SessionData::new("user-1")))
    .await;
  assert!(matches!(seeded, Exit::Success(())), "seed session");

  let cors = CorsConfig::new()
    .allow_origin("https://app.example")
    .allow_methods([Method::GET, Method::POST])
    .allow_headers([
      header::CONTENT_TYPE,
      header::HeaderName::from_static(CSRF_HEADER),
    ])
    .allow_credentials(true)
    .build()
    .expect("cors");

  let app = Router::new()
    .route("/", routing::get(session_user))
    .route(
      "/notes",
      axum::routing::get(|token: CsrfToken| async move { token.as_str().to_owned() })
        .post(|| async { "saved" }),
    )
    .layer(SecurityLayer::recommended(key))
    .layer(cors)
    .with_state(hlist![sessions]);

  // A safe request: the effect handler runs; the response carries the security headers.
  let res = send(
    &app,
    Request::builder().uri("/").body(Body::empty()).unwrap(),
  )
  .await;
  assert_eq!(res.headers()[header::X_FRAME_OPTIONS], "DENY");
  assert!(res.headers().contains_key(header::CONTENT_SECURITY_POLICY));
  assert_eq!(body_text(res).await, "user=user-1");

  // A POST without the session's CSRF token is refused.
  let res = send(
    &app,
    Request::builder()
      .method("POST")
      .uri("/notes")
      .body(Body::empty())
      .unwrap(),
  )
  .await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);

  // A browser session: the signed cookie and its token, echoed back on the POST.
  let res = send(
    &app,
    Request::builder()
      .uri("/notes")
      .body(Body::empty())
      .unwrap(),
  )
  .await;
  let cookie = res.headers()[header::SET_COOKIE]
    .to_str()
    .unwrap()
    .split(';')
    .next()
    .unwrap()
    .to_owned();
  let token = body_text(res).await;
  let res = send(
    &app,
    Request::builder()
      .method("POST")
      .uri("/notes")
      .header(header::COOKIE, cookie)
      .header(CSRF_HEADER, token)
      .body(Body::empty())
      .unwrap(),
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_text(res).await, "saved");
  println!("040_app_host ok");
}
