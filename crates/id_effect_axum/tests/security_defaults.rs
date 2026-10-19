//! 0.11 secure defaults of `SecurityLayer` (E11.2): bounded CSRF form reads (M4), masked CSRF
//! tokens (L1), CSRF rotation (L5) and the cross-origin isolation headers (L2).

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use id_effect_axum::server::security::{
  CSRF_COOKIE, CSRF_FORM_FIELD, CSRF_HEADER, CookieKey, CsrfConfig, CsrfToken, SecurityHeaders,
  SecurityLayer,
};
use tower::ServiceExt;

fn key() -> CookieKey {
  CookieKey::from_bytes(&[7; 64]).expect("64-byte key")
}

/// `GET /` renders the token, `POST /` echoes the body, `POST /login` rotates the token.
fn app(layer: SecurityLayer) -> Router {
  Router::new()
    .route(
      "/",
      get(|token: CsrfToken| async move { token.as_str().to_owned() })
        .post(|body: String| async move { body }),
    )
    .route(
      "/login",
      post(|token: CsrfToken| async move { (token.rotate(), "logged in") }),
    )
    .layer(layer)
}

async fn send(app: &Router, request: Request<Body>) -> Response {
  app.clone().oneshot(request).await.unwrap()
}

async fn body_string(response: Response) -> String {
  let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
    .await
    .unwrap();
  String::from_utf8(bytes.to_vec()).unwrap()
}

/// The `name=value` pair of the CSRF `Set-Cookie` header, if one was sent.
fn csrf_cookie_pair(response: &Response) -> Option<String> {
  response
    .headers()
    .get_all(header::SET_COOKIE)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .find(|v| v.starts_with(CSRF_COOKIE))
    .map(|v| v.split(';').next().unwrap().to_owned())
}

fn get_root(cookie: Option<&str>) -> Request<Body> {
  let mut builder = Request::builder().uri("/");
  if let Some(cookie) = cookie {
    builder = builder.header(header::COOKIE, cookie);
  }
  builder.body(Body::empty()).unwrap()
}

/// A browser session: (cookie pair, token).
async fn session(app: &Router) -> (String, String) {
  let response = send(app, get_root(None)).await;
  assert_eq!(response.status(), StatusCode::OK);
  let cookie = csrf_cookie_pair(&response).expect("csrf cookie issued");
  (cookie, body_string(response).await)
}

fn post_with(uri: &str, cookie: &str, token: &str) -> Request<Body> {
  Request::builder()
    .method("POST")
    .uri(uri)
    .header(header::COOKIE, cookie)
    .header(CSRF_HEADER, token)
    .body(Body::empty())
    .unwrap()
}

fn post_form(cookie: &str, body: Body) -> Request<Body> {
  Request::builder()
    .method("POST")
    .uri("/")
    .header(header::COOKIE, cookie)
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .body(body)
    .unwrap()
}

/// A body that never yields a frame (a client that stalls mid-upload).
struct Stalled;

impl http_body::Body for Stalled {
  type Data = Bytes;
  type Error = Infallible;

  fn poll_frame(
    self: Pin<&mut Self>,
    _cx: &mut Context<'_>,
  ) -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
    Poll::Pending
  }
}

// ---- M4: bounded form read ------------------------------------------------------------------

#[tokio::test]
async fn m4_csrf_form_read_respects_limit() {
  // Default 64 KiB: a 100 KiB form is refused before the handler, whatever wraps the layer.
  let app_default = app(SecurityLayer::recommended(key()));
  let (cookie, token) = session(&app_default).await;
  let big = format!("{CSRF_FORM_FIELD}={token}&pad={}", "a".repeat(100 * 1024));
  let res = send(&app_default, post_form(&cookie, Body::from(big.clone()))).await;
  assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

  // A declared Content-Length over the limit is refused without reading.
  let declared = Request::builder()
    .method("POST")
    .uri("/")
    .header(header::COOKIE, &cookie)
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::CONTENT_LENGTH, big.len())
    .body(Body::new(Stalled))
    .unwrap();
  let res = send(&app_default, declared).await;
  assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

  // A form under the limit passes and reaches the handler intact.
  let small = format!("title=hi&{CSRF_FORM_FIELD}={token}");
  let res = send(&app_default, post_form(&cookie, Body::from(small.clone()))).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_string(res).await, small);

  // The limit is configurable.
  let tight =
    app(SecurityLayer::recommended(key()).csrf_config(CsrfConfig::default().form_limit(64)));
  let res = send(&tight, post_form(&cookie, Body::from(small))).await;
  assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);

  // Non-form bodies are never read: the header is the only source.
  let json = Request::builder()
    .method("POST")
    .uri("/")
    .header(header::COOKIE, &cookie)
    .header(header::CONTENT_TYPE, "application/json")
    .body(Body::new(Stalled))
    .unwrap();
  let res = send(&app_default, json).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn m4_csrf_form_read_times_out() {
  let config = CsrfConfig::recommended().read_timeout(Duration::from_millis(50));
  let app = app(SecurityLayer::recommended(key()).csrf_config(config));
  let (cookie, _) = session(&app).await;
  let res = tokio::time::timeout(
    Duration::from_secs(5),
    send(&app, post_form(&cookie, Body::new(Stalled))),
  )
  .await
  .expect("the layer gives up on its own");
  assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT);
}

// ---- L1: masked token -----------------------------------------------------------------------

#[tokio::test]
async fn l1_csrf_token_masked_differently_per_response() {
  let app = app(SecurityLayer::recommended(key()));
  let (cookie, first) = session(&app).await;
  let mut seen = vec![first];
  for _ in 0..3 {
    let res = send(&app, get_root(Some(&cookie))).await;
    assert!(
      csrf_cookie_pair(&res).is_none(),
      "same session, no new cookie"
    );
    seen.push(body_string(res).await);
  }
  for (i, a) in seen.iter().enumerate() {
    for b in &seen[i + 1..] {
      assert_ne!(a, b);
    }
  }
  // The cookie's secret (the 43 base64url characters after the signature) never appears as such
  // in a rendered token.
  let signed = cookie.split_once('=').unwrap().1;
  let secret = &signed[signed.len() - 43..];
  assert!(seen.iter().all(|token| !token.contains(secret)));
  // The header form is masked too.
  let header_app = Router::new()
    .route("/", get(|token: CsrfToken| async move { (token, "ok") }))
    .layer(SecurityLayer::recommended(key()));
  let a = send(&header_app, get_root(Some(&cookie))).await;
  let b = send(&header_app, get_root(Some(&cookie))).await;
  assert_ne!(a.headers()[CSRF_HEADER], b.headers()[CSRF_HEADER]);
}

#[tokio::test]
async fn l1_masked_token_validates() {
  let app = app(SecurityLayer::recommended(key()));
  let (cookie, first) = session(&app).await;
  let second = body_string(send(&app, get_root(Some(&cookie))).await).await;
  for token in [&first, &second] {
    let res = send(&app, post_with("/", &cookie, token)).await;
    assert_eq!(res.status(), StatusCode::OK, "{token}");
  }
  // Tampering with either half breaks it; another session's token never fits.
  let mut tampered = first.clone().into_bytes();
  tampered[70] = if tampered[70] == b'A' { b'B' } else { b'A' };
  let tampered = String::from_utf8(tampered).unwrap();
  let res = send(&app, post_with("/", &cookie, &tampered)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
  let (_, other) = session(&app).await;
  let res = send(&app, post_with("/", &cookie, &other)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ---- L5: rotation ---------------------------------------------------------------------------

#[tokio::test]
async fn l5_rotate_issues_new_token_and_invalidates_old() {
  let app = app(SecurityLayer::recommended(key()));
  let (old_cookie, old_token) = session(&app).await;

  let login = send(&app, post_with("/login", &old_cookie, &old_token)).await;
  assert_eq!(login.status(), StatusCode::OK);
  let new_cookie = csrf_cookie_pair(&login).expect("rotation sets a new cookie");
  assert_ne!(new_cookie, old_cookie);
  let new_token = login.headers()[CSRF_HEADER].to_str().unwrap().to_owned();
  assert_eq!(
    login.headers().get_all(header::SET_COOKIE).iter().count(),
    1,
    "one CSRF cookie"
  );

  // The browser now holds the new cookie: the old token no longer validates with it.
  let res = send(&app, post_with("/", &new_cookie, &old_token)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
  // The new token does not fit the old cookie either: it carries a new secret.
  let res = send(&app, post_with("/", &old_cookie, &new_token)).await;
  assert_eq!(res.status(), StatusCode::FORBIDDEN);
  let res = send(&app, post_with("/", &new_cookie, &new_token)).await;
  assert_eq!(res.status(), StatusCode::OK);

  // Rotating on a brand-new session replaces the cookie issued in the same response.
  let fresh = Router::new()
    .route(
      "/",
      get(|token: CsrfToken| async move { token.rotate().as_str().to_owned() }),
    )
    .layer(SecurityLayer::recommended(key()));
  let res = send(&fresh, get_root(None)).await;
  assert_eq!(res.headers().get_all(header::SET_COOKIE).iter().count(), 1);
  let cookie = csrf_cookie_pair(&res).unwrap();
  let token = body_string(res).await;
  let res = send(&app, post_with("/", &cookie, &token)).await;
  assert_eq!(res.status(), StatusCode::OK);
}

// ---- L2: isolation headers ------------------------------------------------------------------

#[tokio::test]
async fn l2_recommended_headers_include_coop_corp_permissions_policy() {
  let app = app(SecurityLayer::recommended(key()));
  let res = send(&app, get_root(None)).await;
  let h = res.headers();
  assert_eq!(h["cross-origin-opener-policy"], "same-origin");
  assert_eq!(h["cross-origin-resource-policy"], "same-origin");
  let policy = h["permissions-policy"].to_str().unwrap();
  for feature in ["camera", "microphone", "geolocation", "payment", "usb"] {
    assert!(policy.contains(&format!("{feature}=()")), "{policy}");
  }

  // Each is overridable through the layer, and a handler's own value wins.
  let headers = SecurityHeaders::recommended()
    .cross_origin_resource_policy("cross-origin")
    .unwrap()
    .without_permissions_policy();
  let custom = Router::new()
    .route(
      "/",
      get(|| async { ([("cross-origin-opener-policy", "unsafe-none")], "ok") }),
    )
    .layer(SecurityLayer::recommended(key()).headers(headers));
  let res = send(&custom, get_root(None)).await;
  let h = res.headers();
  assert_eq!(h["cross-origin-opener-policy"], "unsafe-none");
  assert_eq!(h["cross-origin-resource-policy"], "cross-origin");
  assert!(h.get("permissions-policy").is_none());
}
