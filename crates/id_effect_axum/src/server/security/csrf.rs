//! CSRF protection (synchronizer token in a signed cookie).
//!
//! Each client session gets a 32-byte CSPRNG token, stored in the signed [`CSRF_COOKIE`]
//! (`__Host-`, `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/`). Handlers read it with the
//! [`CsrfToken`] extractor and render it into pages; the client echoes it on every
//! state-changing request in the [`CSRF_HEADER`] header or the [`CSRF_FORM_FIELD`] field of an
//! `application/x-www-form-urlencoded` body. The echoed value is compared with the cookie's in
//! constant time ([`subtle::ConstantTimeEq`]). A forged or unsigned cookie never counts.
//!
//! Multipart bodies are not inspected: send the header from such forms.

use std::convert::Infallible;
use std::fmt;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, IntoResponseParts, Response, ResponseParts};
use cookie::{Cookie, CookieJar, SameSite};
use subtle::ConstantTimeEq;

use super::cookies::{CookieKey, request_jar};

/// CSRF token header name expected on mutating requests.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Name of the signed cookie carrying the session's CSRF token.
pub const CSRF_COOKIE: &str = "__Host-csrf";

/// Form field carrying the CSRF token in `application/x-www-form-urlencoded` bodies.
pub const CSRF_FORM_FIELD: &str = "csrf_token";

/// Token entropy in bytes.
const TOKEN_BYTES: usize = 32;

/// Largest form body read to find [`CSRF_FORM_FIELD`] (axum's default body limit).
const FORM_LIMIT: usize = 2 * 1024 * 1024;

/// Constant-time token equality (lengths are not secret).
fn tokens_match(expected: &str, submitted: &str) -> bool {
  expected.as_bytes().ct_eq(submitted.as_bytes()).into()
}

fn is_safe(method: &Method) -> bool {
  matches!(
    *method,
    Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
  )
}

fn forbidden() -> Response {
  (StatusCode::FORBIDDEN, "csrf token mismatch").into_response()
}

/// This session's CSRF token. Render it into forms ([`CSRF_FORM_FIELD`]) or hand it to scripts;
/// returning it from a handler sets the [`CSRF_HEADER`] response header.
#[derive(Clone, PartialEq, Eq)]
pub struct CsrfToken(Arc<str>);

impl CsrfToken {
  fn generate() -> Self {
    Self(super::random_token(TOKEN_BYTES).into())
  }

  /// The token text (base64url).
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for CsrfToken {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("CsrfToken(<redacted>)")
  }
}

impl<S> FromRequestParts<S> for CsrfToken
where
  S: Send + Sync,
{
  type Rejection = (StatusCode, &'static str);

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    parts.extensions.get::<CsrfToken>().cloned().ok_or((
      StatusCode::INTERNAL_SERVER_ERROR,
      "no CSRF token: SecurityLayer with CSRF is not installed",
    ))
  }
}

impl IntoResponseParts for CsrfToken {
  type Error = Infallible;

  fn into_response_parts(self, mut res: ResponseParts) -> Result<ResponseParts, Self::Error> {
    if let Ok(value) = HeaderValue::from_str(self.as_str()) {
      res.headers_mut().insert(CSRF_HEADER, value);
    }
    Ok(res)
  }
}

/// The CSRF part of [`SecurityLayer`](super::SecurityLayer).
#[derive(Clone, Debug)]
pub(crate) struct CsrfProtection {
  key: CookieKey,
  exempt: Vec<String>,
}

impl CsrfProtection {
  pub(crate) fn new(key: CookieKey) -> Self {
    Self {
      key,
      exempt: Vec::new(),
    }
  }

  pub(crate) fn exempt(&mut self, prefix: String) {
    let prefix = prefix.trim_end_matches('/').to_owned();
    self.exempt.push(prefix);
  }

  fn is_exempt(&self, path: &str) -> bool {
    self.exempt.iter().any(|prefix| {
      prefix.is_empty()
        || path
          .strip_prefix(prefix.as_str())
          .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
  }

  /// The session token from a correctly signed cookie, if any.
  fn session_token(&self, headers: &HeaderMap) -> Option<CsrfToken> {
    let jar = request_jar(headers);
    let cookie = jar.signed(self.key.key()).get(CSRF_COOKIE)?;
    let value = cookie.value();
    (!value.is_empty()).then(|| CsrfToken(value.into()))
  }

  fn set_cookie(&self, token: &CsrfToken) -> Option<HeaderValue> {
    let cookie = Cookie::build((CSRF_COOKIE, token.as_str().to_owned()))
      .path("/")
      .secure(true)
      .http_only(true)
      .same_site(SameSite::Strict)
      .build();
    let mut jar = CookieJar::new();
    jar.signed_mut(self.key.key()).add(cookie);
    let signed = jar.get(CSRF_COOKIE)?;
    HeaderValue::from_str(&signed.to_string()).ok()
  }

  /// Resolve (or issue) the session token, store it in the request extensions and check
  /// unsafe requests. Returns the request to run or the rejection, and the `Set-Cookie` value
  /// to send when a new token was issued.
  pub(crate) async fn guard(
    &self,
    mut request: Request,
  ) -> (Result<Request, Response>, Option<HeaderValue>) {
    let existing = self.session_token(request.headers());
    let issued = existing.is_none();
    let token = existing.unwrap_or_else(CsrfToken::generate);
    let set_cookie = if issued {
      self.set_cookie(&token)
    } else {
      None
    };
    request.extensions_mut().insert(token.clone());
    if is_safe(request.method()) || self.is_exempt(request.uri().path()) {
      return (Ok(request), set_cookie);
    }
    if issued {
      // No valid session cookie: nothing the client sends can match.
      return (Err(forbidden()), set_cookie);
    }
    let (request, submitted) = submitted_token(request).await;
    match submitted {
      Some(submitted) if tokens_match(token.as_str(), &submitted) => (Ok(request), None),
      _ => (Err(forbidden()), None),
    }
  }
}

/// The token echoed in [`CSRF_HEADER`], else in a urlencoded form body's [`CSRF_FORM_FIELD`]
/// (the body is buffered and put back).
async fn submitted_token(request: Request) -> (Request, Option<String>) {
  if let Some(value) = request.headers().get(CSRF_HEADER) {
    let value = value.to_str().ok().map(str::to_owned);
    return (request, value);
  }
  let is_form = request
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
  if !is_form {
    return (request, None);
  }
  let (parts, body) = request.into_parts();
  match axum::body::to_bytes(body, FORM_LIMIT).await {
    Ok(bytes) => {
      let token = form_field(&bytes, CSRF_FORM_FIELD);
      (Request::from_parts(parts, Body::from(bytes)), token)
    }
    Err(_) => (Request::from_parts(parts, Body::empty()), None),
  }
}

fn form_field(body: &[u8], name: &str) -> Option<String> {
  body.split(|b| *b == b'&').find_map(|pair| {
    let mut kv = pair.splitn(2, |b| *b == b'=');
    let key = percent_decode(kv.next()?)?;
    if key != name {
      return None;
    }
    percent_decode(kv.next().unwrap_or_default())
  })
}

fn percent_decode(raw: &[u8]) -> Option<String> {
  let mut out = Vec::with_capacity(raw.len());
  let mut i = 0;
  while let Some(&b) = raw.get(i) {
    match b {
      b'+' => out.push(b' '),
      b'%' => {
        let hex = std::str::from_utf8(raw.get(i + 1..i + 3)?).ok()?;
        out.push(u8::from_str_radix(hex, 16).ok()?);
        i += 2;
      }
      _ => out.push(b),
    }
    i += 1;
  }
  String::from_utf8(out).ok()
}

/// Shared static CSRF token (0.7).
#[deprecated(
  since = "0.8.0",
  note = "a static shared token is not per-session; use `SecurityLayer::recommended(key)`"
)]
#[derive(Clone)]
pub struct CsrfConfig {
  token: Arc<str>,
}

#[allow(deprecated)]
impl CsrfConfig {
  /// Use `token` for validation on POST/PUT/PATCH/DELETE.
  #[deprecated(
    since = "0.8.0",
    note = "a static shared token is not per-session; use `SecurityLayer::recommended(key)`"
  )]
  pub fn new(token: impl Into<Arc<str>>) -> Self {
    Self {
      token: token.into(),
    }
  }

  /// Expected header value.
  pub fn token(&self) -> &str {
    &self.token
  }
}

/// Reject POST/PUT/PATCH/DELETE when `X-CSRF-Token` does not match (constant-time compare).
#[deprecated(
  since = "0.8.0",
  note = "a static shared token is not per-session; use `SecurityLayer::recommended(key)`"
)]
#[allow(deprecated)]
pub async fn csrf_middleware(
  State(config): State<CsrfConfig>,
  request: Request,
  next: Next,
) -> Response {
  if matches!(
    *request.method(),
    Method::POST | Method::PUT | Method::PATCH | Method::DELETE
  ) {
    let valid = request
      .headers()
      .get(CSRF_HEADER)
      .and_then(|v| v.to_str().ok())
      .is_some_and(|submitted| tokens_match(config.token(), submitted));
    if !valid {
      return forbidden();
    }
  }
  next.run(request).await
}

/// Set CSRF token response header.
#[deprecated(
  since = "0.8.0",
  note = "return the `CsrfToken` extractor from the handler (it sets the header)"
)]
pub fn set_csrf_header(headers: &mut HeaderMap, token: &str) {
  if let Ok(value) = HeaderValue::from_str(token) {
    headers.insert(CSRF_HEADER, value);
  }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
  use super::*;
  use axum::Router;
  use axum::routing::{delete, get, patch, post};
  use tower::ServiceExt;

  fn guarded(config: CsrfConfig) -> Router {
    Router::new()
      .route(
        "/",
        get(|| async { "ok" })
          .post(|| async { "ok" })
          .put(|| async { "ok" })
          .patch(|| async { "ok" })
          .delete(|| async { "ok" }),
      )
      .layer(axum::middleware::from_fn_with_state(
        config,
        csrf_middleware,
      ))
  }

  async fn status(app: Router, method: &str, token: Option<&str>) -> StatusCode {
    let mut req = Request::builder().method(method).uri("/");
    if let Some(token) = token {
      req = req.header(CSRF_HEADER, token);
    }
    app
      .oneshot(req.body(Body::empty()).unwrap())
      .await
      .unwrap()
      .status()
  }

  #[test]
  fn tokens_match_is_exact() {
    assert!(tokens_match("abc", "abc"));
    assert!(!tokens_match("abc", "abd"));
    assert!(!tokens_match("abc", "ab"));
    assert!(!tokens_match("abc", ""));
  }

  #[test]
  fn exempt_prefix_matches_whole_segments() {
    let mut csrf = CsrfProtection::new(CookieKey::generate());
    csrf.exempt("/webhooks/".into());
    assert!(csrf.is_exempt("/webhooks"));
    assert!(csrf.is_exempt("/webhooks/stripe"));
    assert!(!csrf.is_exempt("/webhooksx"));
    assert!(!csrf.is_exempt("/api/webhooks"));
  }

  #[test]
  fn form_field_decodes_percent_and_plus() {
    let body = b"title=a+b&csrf_token=x%2Dy_z&other=1";
    assert_eq!(form_field(body, "csrf_token").as_deref(), Some("x-y_z"));
    assert_eq!(form_field(body, "title").as_deref(), Some("a b"));
    assert_eq!(form_field(body, "missing"), None);
    assert_eq!(form_field(b"csrf_token=%zz", "csrf_token"), None);
    assert_eq!(form_field(b"csrf_token", "csrf_token").as_deref(), Some(""));
  }

  #[test]
  fn csrf_token_debug_is_redacted() {
    assert_eq!(
      format!("{:?}", CsrfToken::generate()),
      "CsrfToken(<redacted>)"
    );
  }

  #[test]
  fn csrf_config_exposes_token() {
    assert_eq!(CsrfConfig::new("secret").token(), "secret");
  }

  #[test]
  fn set_csrf_header_inserts_value() {
    let mut headers = HeaderMap::new();
    set_csrf_header(&mut headers, "tok");
    assert_eq!(headers.get(CSRF_HEADER).unwrap(), "tok");
  }

  #[tokio::test]
  async fn csrf_middleware_blocks_post_without_token() {
    let app = guarded(CsrfConfig::new("secret"));
    assert_eq!(status(app, "POST", None).await, StatusCode::FORBIDDEN);
  }

  #[tokio::test]
  async fn csrf_middleware_rejects_wrong_token_constant_time_path() {
    let app = guarded(CsrfConfig::new("secret"));
    assert_eq!(
      status(app.clone(), "POST", Some("secreT")).await,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      status(app, "POST", Some("secret-longer")).await,
      StatusCode::FORBIDDEN
    );
  }

  #[tokio::test]
  async fn csrf_allows_put_with_matching_token() {
    let app = guarded(CsrfConfig::new("secret"));
    assert_eq!(status(app, "PUT", Some("secret")).await, StatusCode::OK);
  }

  #[tokio::test]
  async fn csrf_allows_get_without_token() {
    let app =
      Router::new()
        .route("/", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
          CsrfConfig::new("secret"),
          csrf_middleware,
        ));
    assert_eq!(status(app, "GET", None).await, StatusCode::OK);
  }

  #[tokio::test]
  async fn csrf_rejects_delete_without_token() {
    let app = Router::new().route("/", delete(|| async { "ok" })).layer(
      axum::middleware::from_fn_with_state(CsrfConfig::new("secret"), csrf_middleware),
    );
    assert_eq!(status(app, "DELETE", None).await, StatusCode::FORBIDDEN);
  }

  #[tokio::test]
  async fn csrf_rejects_patch_without_token() {
    let app = Router::new().route("/", patch(|| async { "ok" })).layer(
      axum::middleware::from_fn_with_state(CsrfConfig::new("secret"), csrf_middleware),
    );
    assert_eq!(status(app, "PATCH", None).await, StatusCode::FORBIDDEN);
  }

  #[tokio::test]
  async fn csrf_token_extractor_requires_layer() {
    let app: Router = Router::new().route("/", post(|_t: CsrfToken| async { "ok" }));
    assert_eq!(
      status(app, "POST", None).await,
      StatusCode::INTERNAL_SERVER_ERROR
    );
  }
}
