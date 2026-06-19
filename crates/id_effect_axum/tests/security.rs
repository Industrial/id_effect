use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::{Router, middleware, routing::get};
use id_effect_axum::server::security::{
  CSRF_HEADER, ContentSecurityPolicy, CsrfConfig, csp_middleware, csrf_middleware,
};
use tower::ServiceExt;

#[test]
fn csp_header_joins_directives() {
  let csp = ContentSecurityPolicy::new()
    .default_src_self()
    .script_src_self();
  let v = csp.to_header_value().expect("csp");
  assert!(v.to_str().unwrap().contains("default-src"));
}

#[tokio::test]
async fn csp_middleware_adds_header() {
  let csp = Arc::new(ContentSecurityPolicy::new().default_src_self());
  let app = Router::new()
    .route("/", get(|| async { "ok" }))
    .layer(middleware::from_fn_with_state(csp, csp_middleware));
  let res = app
    .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert!(res.headers().get(header::CONTENT_SECURITY_POLICY).is_some());
}

#[tokio::test]
async fn csrf_rejects_missing_token_on_post() {
  let cfg = CsrfConfig::new("secret");
  let app = Router::new()
    .route("/", get(|| async { "ok" }).post(|| async { "posted" }))
    .layer(middleware::from_fn_with_state(cfg, csrf_middleware));
  let res = app
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn csrf_accepts_matching_token() {
  let cfg = CsrfConfig::new("secret");
  let app = Router::new()
    .route("/", get(|| async { "ok" }).post(|| async { "posted" }))
    .layer(middleware::from_fn_with_state(cfg, csrf_middleware));
  let res = app
    .oneshot(
      Request::builder()
        .method("POST")
        .uri("/")
        .header(CSRF_HEADER, "secret")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
}
