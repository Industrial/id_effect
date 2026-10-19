use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use id_effect::{Effect, Exit, HCons, HNil, need};
use id_effect_axum::server::security::cookies::{Cookie, SameSite};
use id_effect_axum::server::security::{
  COOKIE_KEY_ENV, CSRF_COOKIE, CSRF_FORM_FIELD, CSRF_HEADER, ContentSecurityPolicy, CookieKey,
  Cookies, CorsConfig, CspNonce, CsrfToken, SecurityError, SecurityHeaders, SecurityLayer,
};
use id_effect_config::{ConfigError, ConfigProvider, MapConfigProvider};
use tower::ServiceExt;

fn key() -> CookieKey {
  CookieKey::from_bytes(&[42; 64]).expect("64-byte key")
}

/// `GET /` returns the CSRF token; `POST /` and `POST /webhooks/stripe` are mutating routes.
fn app(layer: SecurityLayer) -> Router {
  Router::new()
    .route(
      "/",
      get(|token: CsrfToken| async move { token.as_str().to_owned() }).post(|| async { "posted" }),
    )
    .route("/webhooks/stripe", post(|| async { "hooked" }))
    .layer(layer)
}

async fn send(app: &Router, request: Request<Body>) -> Response {
  app.clone().oneshot(request).await.unwrap()
}

fn get_root() -> Request<Body> {
  Request::builder().uri("/").body(Body::empty()).unwrap()
}

async fn body_string(response: Response) -> String {
  let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
    .await
    .unwrap();
  String::from_utf8(bytes.to_vec()).unwrap()
}

/// The `name=value` pair of the CSRF `Set-Cookie` header.
fn csrf_cookie_pair(response: &Response) -> String {
  let set_cookie = response
    .headers()
    .get_all(header::SET_COOKIE)
    .iter()
    .map(|v| v.to_str().unwrap())
    .find(|v| v.starts_with(CSRF_COOKIE))
    .expect("csrf cookie issued");
  set_cookie.split(';').next().unwrap().to_owned()
}

/// A browser session: (cookie pair, token).
async fn session(app: &Router) -> (String, String) {
  let response = send(app, get_root()).await;
  assert_eq!(response.status(), StatusCode::OK);
  let cookie = csrf_cookie_pair(&response);
  (cookie, body_string(response).await)
}

fn post_with(cookie: Option<&str>, token: Option<&str>) -> Request<Body> {
  let mut builder = Request::builder().method("POST").uri("/");
  if let Some(cookie) = cookie {
    builder = builder.header(header::COOKIE, cookie);
  }
  if let Some(token) = token {
    builder = builder.header(CSRF_HEADER, token);
  }
  builder.body(Body::empty()).unwrap()
}

// ---- 0.7 API (deprecated shims) -------------------------------------------------------------

#[test]
fn csp_header_joins_directives() {
  let csp = ContentSecurityPolicy::new()
    .default_src_self()
    .script_src_self();
  let v = csp.to_header_value().expect("csp");
  assert!(v.to_str().unwrap().contains("default-src"));
}

#[tokio::test]
#[allow(deprecated)]
async fn csp_middleware_adds_header() {
  use id_effect_axum::server::security::csp_middleware;
  let csp = Arc::new(ContentSecurityPolicy::new().default_src_self());
  let app = Router::new()
    .route("/", get(|| async { "ok" }))
    .layer(axum::middleware::from_fn_with_state(csp, csp_middleware));
  let res = send(&app, get_root()).await;
  assert!(res.headers().get(header::CONTENT_SECURITY_POLICY).is_some());
}

#[tokio::test]
#[allow(deprecated)]
async fn csrf_rejects_missing_token_on_post() {
  use id_effect_axum::server::security::{CsrfConfig, csrf_middleware};
  let cfg = CsrfConfig::new("secret");
  let app = Router::new()
    .route("/", get(|| async { "ok" }).post(|| async { "posted" }))
    .layer(axum::middleware::from_fn_with_state(cfg, csrf_middleware));
  let res = send(&app, post_with(None, None)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[allow(deprecated)]
async fn csrf_accepts_matching_token() {
  use id_effect_axum::server::security::{CsrfConfig, csrf_middleware};
  let cfg = CsrfConfig::new("secret");
  let app = Router::new()
    .route("/", get(|| async { "ok" }).post(|| async { "posted" }))
    .layer(axum::middleware::from_fn_with_state(cfg, csrf_middleware));
  let res = send(&app, post_with(None, Some("secret"))).await;
  assert_eq!(res.status(), StatusCode::OK);
}

// ---- CSRF (#1, #2) --------------------------------------------------------------------------

#[tokio::test]
async fn csrf_tokens_are_random_per_session() {
  let app = app(SecurityLayer::recommended(key()));
  let (cookie_a, token_a) = session(&app).await;
  let (cookie_b, token_b) = session(&app).await;
  assert_ne!(token_a, token_b);
  assert_ne!(cookie_a, cookie_b);
  let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
    .decode(&token_a)
    .unwrap();
  // Masked: a 32-byte pad, then the pad XOR the 32-byte secret.
  assert_eq!(raw.len(), 64);
  // The same session keeps its secret and is not re-issued a cookie.
  let again = send(
    &app,
    Request::builder()
      .uri("/")
      .header(header::COOKIE, &cookie_a)
      .body(Body::empty())
      .unwrap(),
  )
  .await;
  assert!(again.headers().get(header::SET_COOKIE).is_none());
  let token_again = body_string(again).await;
  assert_ne!(token_again, token_a, "re-masked per response");
  let res = send(&app, post_with(Some(&cookie_a), Some(&token_again))).await;
  assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn csrf_cookie_is_host_prefixed_strict_secure() {
  let app = app(SecurityLayer::recommended(key()));
  let response = send(&app, get_root()).await;
  let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
  assert!(set_cookie.starts_with("__Host-csrf="));
  for attribute in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
    assert!(set_cookie.contains(attribute), "{set_cookie}");
  }
  assert!(!set_cookie.contains("Domain"));
}

#[tokio::test]
async fn csrf_rejects_mismatched_and_forged_token() {
  let app = app(SecurityLayer::recommended(key()));
  let (cookie, token) = session(&app).await;
  let (_, other_token) = session(&app).await;

  // Valid session cookie, another session's token.
  let res = send(&app, post_with(Some(&cookie), Some(&other_token))).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);

  // Forged (unsigned) cookie chosen by the attacker, echoed in the header.
  let forged = format!("{CSRF_COOKIE}={token}");
  let res = send(&app, post_with(Some(&forged), Some(&token))).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);

  // Cookie signed with a different key.
  let foreign = app_with_other_key().await;
  let res = send(&app, post_with(Some(&foreign.0), Some(&foreign.1))).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);

  // No cookie at all, and a cookie without the token.
  let res = send(&app, post_with(None, Some(&token))).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
  let res = send(&app, post_with(Some(&cookie), None)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);

  // The genuine pair passes.
  let res = send(&app, post_with(Some(&cookie), Some(&token))).await;
  assert_eq!(res.status(), StatusCode::OK);
}

async fn app_with_other_key() -> (String, String) {
  let other = app(SecurityLayer::recommended(CookieKey::generate()));
  session(&other).await
}

#[tokio::test]
async fn csrf_accepts_form_field_and_keeps_body() {
  let app = Router::new()
    .route(
      "/",
      get(|token: CsrfToken| async move { token.as_str().to_owned() })
        .post(|body: String| async move { body }),
    )
    .layer(SecurityLayer::recommended(key()));
  let (cookie, token) = session(&app).await;
  let form = format!("title=hi&{CSRF_FORM_FIELD}={token}");
  let res = send(
    &app,
    Request::builder()
      .method("POST")
      .uri("/")
      .header(header::COOKIE, &cookie)
      .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
      .body(Body::from(form.clone()))
      .unwrap(),
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_string(res).await, form);
}

#[tokio::test]
async fn recommended_layer_blocks_post_without_token() {
  let app = app(SecurityLayer::recommended(key()));
  for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
    let res = send(
      &app,
      Request::builder()
        .method(method.clone())
        .uri("/")
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "{method}");
    // Rejections still carry the security headers and a fresh session cookie.
    assert!(res.headers().get(header::X_FRAME_OPTIONS).is_some());
    assert!(res.headers().get(header::SET_COOKIE).is_some());
  }
}

#[tokio::test]
async fn recommended_layer_allows_exempt_prefix() {
  let app = app(SecurityLayer::recommended(key()).csrf_exempt("/webhooks"));
  let hook = Request::builder()
    .method("POST")
    .uri("/webhooks/stripe")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, hook).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_string(res).await, "hooked");
  // Non-exempt routes stay protected.
  let res = send(&app, post_with(None, None)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn without_csrf_allows_post() {
  let app = Router::new()
    .route("/", post(|| async { "posted" }))
    .layer(SecurityLayer::recommended(key()).dangerous_without_csrf());
  let res = send(&app, post_with(None, None)).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert!(res.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn csrf_token_response_part_sets_header() {
  let app = Router::new()
    .route("/", get(|token: CsrfToken| async move { (token, "ok") }))
    .layer(SecurityLayer::recommended(key()));
  let res = send(&app, get_root()).await;
  let header_token = res.headers()[CSRF_HEADER].to_str().unwrap().to_owned();
  assert_eq!(header_token.len(), 86);
}

// ---- CORS (#3) ------------------------------------------------------------------------------

fn cors_app() -> Router {
  let cors = CorsConfig::new()
    .allow_origin("https://app.example")
    .allow_methods([Method::GET, Method::POST])
    .allow_headers([header::CONTENT_TYPE])
    .allow_credentials(true)
    .build()
    .expect("valid cors");
  Router::new()
    .route("/", get(|| async { "ok" }).post(|| async { "posted" }))
    .layer(cors)
}

#[test]
fn cors_rejects_credentials_with_wildcard_origin() {
  assert_eq!(
    CorsConfig::new()
      .allow_any_origin()
      .allow_credentials(true)
      .build()
      .unwrap_err(),
    SecurityError::CorsCredentialsWithWildcard
  );
  assert!(CorsConfig::new().allow_any_origin().build().is_ok());
}

#[test]
fn cors_rejects_invalid_origin() {
  assert!(matches!(
    CorsConfig::new()
      .allow_origin("https://app.example/path")
      .build(),
    Err(SecurityError::InvalidCorsOrigin { .. })
  ));
}

#[tokio::test]
async fn cors_preflight_allows_listed_origin() {
  let res = send(
    &cors_app(),
    Request::builder()
      .method("OPTIONS")
      .uri("/")
      .header(header::ORIGIN, "https://app.example")
      .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
      .body(Body::empty())
      .unwrap(),
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);
  let h = res.headers();
  assert_eq!(
    h[header::ACCESS_CONTROL_ALLOW_ORIGIN],
    "https://app.example"
  );
  assert_eq!(h[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
  assert!(
    h[header::ACCESS_CONTROL_ALLOW_METHODS]
      .to_str()
      .unwrap()
      .contains("POST")
  );
}

#[tokio::test]
async fn cors_omits_allow_origin_for_unlisted_origin() {
  let res = send(
    &cors_app(),
    Request::builder()
      .uri("/")
      .header(header::ORIGIN, "https://evil.example")
      .body(Body::empty())
      .unwrap(),
  )
  .await;
  assert!(
    res
      .headers()
      .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
      .is_none()
  );
}

// ---- CSP (#4, #5) ---------------------------------------------------------------------------

#[tokio::test]
async fn csp_nonce_is_per_request_and_matches_header() {
  let app = Router::new()
    .route("/", get(|nonce: CspNonce| async move { nonce.to_string() }))
    .layer(SecurityLayer::recommended(key()));
  let mut nonces = Vec::new();
  for _ in 0..2 {
    let res = send(&app, get_root()).await;
    let csp = res.headers()[header::CONTENT_SECURITY_POLICY]
      .to_str()
      .unwrap()
      .to_owned();
    let nonce = body_string(res).await;
    assert!(csp.contains(&format!("'nonce-{nonce}'")), "{csp}");
    assert!(csp.contains("object-src 'none'"));
    nonces.push(nonce);
  }
  assert_ne!(nonces[0], nonces[1]);
}

#[tokio::test]
async fn csp_custom_policy_with_script_hash() {
  let policy = ContentSecurityPolicy::new()
    .default_src(&["'none'"])
    .script_hash("alert(1)");
  let layer = SecurityLayer::recommended(key())
    .csp(policy)
    .expect("valid csp");
  let app = Router::new()
    .route("/", get(|| async { "ok" }))
    .layer(layer);
  let res = send(&app, get_root()).await;
  let csp = res.headers()[header::CONTENT_SECURITY_POLICY]
    .to_str()
    .unwrap();
  assert!(csp.starts_with("default-src 'none'; script-src 'sha256-"));
  assert!(!csp.contains("nonce"));
}

#[test]
fn csp_invalid_policy_is_a_typed_error() {
  let bad = ContentSecurityPolicy::new().script_src(&["'self';"]);
  assert!(matches!(
    SecurityLayer::recommended(key()).csp(bad),
    Err(SecurityError::InvalidCspDirective { .. })
  ));
}

// ---- Default headers (#6) -------------------------------------------------------------------

#[tokio::test]
async fn default_security_headers_present() {
  let app = app(SecurityLayer::recommended(key()));
  let res = send(&app, get_root()).await;
  let h = res.headers();
  assert_eq!(
    h[header::STRICT_TRANSPORT_SECURITY],
    "max-age=63072000; includeSubDomains"
  );
  assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
  assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
  assert_eq!(
    h[header::REFERRER_POLICY],
    "strict-origin-when-cross-origin"
  );
  assert!(h.get(header::CONTENT_SECURITY_POLICY).is_some());
}

#[tokio::test]
async fn handler_header_overrides_default() {
  let app = Router::new()
    .route(
      "/",
      get(|| async {
        (
          [
            (header::X_FRAME_OPTIONS, "SAMEORIGIN"),
            (header::CONTENT_SECURITY_POLICY, "default-src *"),
          ],
          "ok",
        )
      }),
    )
    .layer(SecurityLayer::recommended(key()));
  let res = send(&app, get_root()).await;
  let h = res.headers();
  assert_eq!(h[header::X_FRAME_OPTIONS], "SAMEORIGIN");
  assert_eq!(h[header::CONTENT_SECURITY_POLICY], "default-src *");
  assert_eq!(h.get_all(header::X_FRAME_OPTIONS).iter().count(), 1);
  assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
}

#[tokio::test]
async fn security_headers_can_be_replaced_or_dropped() {
  let app = Router::new().route("/", get(|| async { "ok" })).layer(
    SecurityLayer::recommended(key())
      .headers(SecurityHeaders::recommended().without_hsts())
      .without_csp(),
  );
  let res = send(&app, get_root()).await;
  assert!(
    res
      .headers()
      .get(header::STRICT_TRANSPORT_SECURITY)
      .is_none()
  );
  assert!(res.headers().get(header::CONTENT_SECURITY_POLICY).is_none());
  assert!(res.headers().get(header::X_FRAME_OPTIONS).is_some());

  let bare = Router::new()
    .route("/", get(|| async { "ok" }))
    .layer(SecurityLayer::recommended(key()).without_security_headers());
  let res = send(&bare, get_root()).await;
  assert!(res.headers().get(header::X_FRAME_OPTIONS).is_none());
}

// ---- Cookies (#7) ---------------------------------------------------------------------------

#[tokio::test]
async fn cookie_defaults_applied_by_extractor() {
  let app = Router::new()
    .route(
      "/",
      get(|mut cookies: Cookies| async move {
        cookies.add(Cookie::new("theme", "dark"));
        cookies.add(
          Cookie::build(("lax", "1"))
            .same_site(SameSite::None)
            .build(),
        );
        (cookies, "ok")
      }),
    )
    .layer(SecurityLayer::recommended(key()));
  let res = send(&app, get_root()).await;
  let set: Vec<&str> = res
    .headers()
    .get_all(header::SET_COOKIE)
    .iter()
    .map(|v| v.to_str().unwrap())
    .collect();
  let theme = set.iter().find(|c| c.starts_with("theme=")).unwrap();
  for attribute in ["HttpOnly", "Secure", "SameSite=Lax", "Path=/"] {
    assert!(theme.contains(attribute), "{theme}");
  }
  let lax = set.iter().find(|c| c.starts_with("lax=")).unwrap();
  assert!(lax.contains("SameSite=None"), "{lax}");
}

#[tokio::test]
async fn cookie_signed_and_private_round_trip() {
  let app = Router::new()
    .route(
      "/set",
      get(|mut cookies: Cookies| async move {
        cookies.add_signed(Cookie::new("uid", "7")).unwrap();
        cookies.add_private(Cookie::new("secret", "s3")).unwrap();
        (cookies, "set")
      }),
    )
    .route(
      "/get",
      get(|cookies: Cookies| async move {
        let uid = cookies.get_signed("uid").map(|c| c.value().to_owned());
        let secret = cookies.get_private("secret").map(|c| c.value().to_owned());
        format!("{uid:?} {secret:?}")
      }),
    )
    .layer(SecurityLayer::recommended(key()).dangerous_without_csrf());
  let res = send(
    &app,
    Request::builder().uri("/set").body(Body::empty()).unwrap(),
  )
  .await;
  let pairs: Vec<String> = res
    .headers()
    .get_all(header::SET_COOKIE)
    .iter()
    .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
    .collect();
  // Exact match: random ciphertext can contain the substring "s3" by chance.
  assert!(
    !pairs.iter().any(|p| p == "secret=s3"),
    "private cookie is encrypted"
  );
  let read = |cookie: String| {
    Request::builder()
      .uri("/get")
      .header(header::COOKIE, cookie)
      .body(Body::empty())
      .unwrap()
  };
  let res = send(&app, read(pairs.join("; "))).await;
  assert_eq!(body_string(res).await, "Some(\"7\") Some(\"s3\")");
  // Tampered or plain cookies do not verify.
  let res = send(&app, read("uid=7; secret=s3".into())).await;
  assert_eq!(body_string(res).await, "None None");
}

#[test]
fn cookie_key_layer_provides_key_from_config() {
  let material = [9u8; 64];
  let provider = MapConfigProvider::from_pairs([(COOKIE_KEY_ENV, STANDARD.encode(material))]);
  let layer = provider
    .layer()
    .provide_to(CookieKey::layer::<ConfigError>());
  let program: Effect<CookieKey, ConfigError, HCons<CookieKey, HNil>> =
    need::<CookieKey>().map(|k: Arc<CookieKey>| (*k).clone());
  let Exit::Success(loaded) = program.provide_layer(layer).block_on() else {
    panic!("cookie key layer failed");
  };
  // The loaded key verifies cookies signed with the same material.
  let mut out = Cookies::from_headers(&Default::default()).with_key(loaded);
  out.add_signed(Cookie::new("a", "b")).unwrap();
  let pair = out.set_cookie_headers()[0]
    .to_str()
    .unwrap()
    .split(';')
    .next()
    .unwrap()
    .to_owned();
  let mut headers = axum::http::HeaderMap::new();
  headers.insert(header::COOKIE, HeaderValue::from_str(&pair).unwrap());
  let back = Cookies::from_headers(&headers).with_key(CookieKey::from_bytes(&material).unwrap());
  assert_eq!(back.get_signed("a").unwrap().value(), "b");
}

#[test]
fn cookie_key_layer_rejects_short_key() {
  let provider = MapConfigProvider::from_pairs([(COOKIE_KEY_ENV, STANDARD.encode([1u8; 16]))]);
  let layer = provider
    .layer()
    .provide_to(CookieKey::layer::<ConfigError>());
  let program: Effect<(), ConfigError, HCons<CookieKey, HNil>> =
    need::<CookieKey>().map(|_k: Arc<CookieKey>| ());
  assert!(matches!(
    program.provide_layer(layer).block_on(),
    Exit::Failure(_)
  ));
}
