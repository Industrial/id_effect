//! CSRF protection (synchronizer token in a signed cookie).
//!
//! Each client session gets a 32-byte CSPRNG secret, stored in the signed [`CSRF_COOKIE`]
//! (`__Host-`, `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/`). Handlers read it with the
//! [`CsrfToken`] extractor and render it into pages; the client echoes it on every
//! state-changing request in the [`CSRF_HEADER`] header or the [`CSRF_FORM_FIELD`] field of an
//! `application/x-www-form-urlencoded` body.
//!
//! The rendered token is masked per response (BREACH): base64url of a fresh 32-byte one-time pad
//! followed by the pad XOR the secret, so no two responses carry the same bytes. The echoed token
//! is unmasked and compared with the cookie's secret in constant time
//! ([`subtle::ConstantTimeEq`]). A forged or unsigned cookie never counts.
//! [`CsrfToken::rotate`] replaces the secret (call it at login).
//!
//! A form body is read only for the urlencoded content type, up to [`CsrfConfig::form_limit`]
//! bytes and within [`CsrfConfig::read_timeout`], whatever other layers wrap this one.
//! Multipart bodies are not inspected: send the header from such forms.

use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, IntoResponseParts, Response, ResponseParts};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
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

/// A session secret.
type Secret = [u8; TOKEN_BYTES];

/// Characters of a secret in the cookie (unpadded base64url of [`TOKEN_BYTES`]).
const SECRET_TEXT_LEN: usize = 43;

/// Characters of the cookie signature (padded base64 of an HMAC-SHA256 tag).
const SIGNATURE_TEXT_LEN: usize = 44;

/// The CSRF cookie's attributes after `name=value`, as `cookie` writes them for a `Secure`,
/// `HttpOnly`, `SameSite=Strict`, `Path=/` cookie.
const COOKIE_ATTRIBUTES: &str = "; HttpOnly; SameSite=Strict; Secure; Path=/";

/// Default largest form body read to find [`CSRF_FORM_FIELD`].
const DEFAULT_FORM_LIMIT: usize = 64 * 1024;

/// Default time allowed to read that form body.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Constant-time token equality (lengths are not secret).
fn tokens_match(expected: &str, submitted: &str) -> bool {
  expected.as_bytes().ct_eq(submitted.as_bytes()).into()
}

/// A fresh session secret from the thread-local CSPRNG.
fn random_secret() -> Secret {
  let mut secret = [0u8; TOKEN_BYTES];
  rand::rng().fill_bytes(&mut secret);
  secret
}

/// `secret` masked with a fresh one-time pad: base64url(pad || pad XOR secret).
fn mask(secret: &[u8]) -> String {
  let mut out = vec![0u8; secret.len() * 2];
  let (pad, masked) = out.split_at_mut(secret.len());
  rand::rng().fill_bytes(pad);
  for ((m, p), s) in masked.iter_mut().zip(pad.iter()).zip(secret) {
    *m = p ^ s;
  }
  URL_SAFE_NO_PAD.encode(out)
}

/// The secret inside a [`mask`]ed token, if it has the masked shape.
fn unmask(token: &str) -> Option<Vec<u8>> {
  let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
  if bytes.len() != 2 * TOKEN_BYTES {
    return None;
  }
  let (pad, masked) = bytes.split_at(TOKEN_BYTES);
  Some(pad.iter().zip(masked).map(|(p, m)| p ^ m).collect())
}

/// Whether the submitted masked token carries `secret` (constant-time compare).
fn masked_matches(secret: &[u8], submitted: &str) -> bool {
  unmask(submitted).is_some_and(|candidate| candidate.as_slice().ct_eq(secret).into())
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

/// Where [`CsrfToken::rotate`] leaves the new secret for the layer to set in the cookie.
#[derive(Clone, Debug, Default)]
pub(crate) struct Rotation(Arc<Mutex<Option<Secret>>>);

impl Rotation {
  fn set(&self, secret: Secret) {
    *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(secret);
  }

  fn take(&self) -> Option<Secret> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
  }
}

/// This session's CSRF token. Render it into forms ([`CSRF_FORM_FIELD`]) or hand it to scripts;
/// returning it from a handler sets the [`CSRF_HEADER`] response header.
///
/// The text is masked per request: it differs on every response and each one validates while the
/// session cookie holds the same secret. Two tokens are equal when they carry the same secret.
#[derive(Clone)]
pub struct CsrfToken {
  masked: Arc<Masked>,
  rotation: Rotation,
}

/// A token's secret and its masked text, masked on first use (a response that renders no token
/// pays for no pad); clones share the text.
struct Masked {
  secret: Secret,
  text: OnceLock<String>,
}

impl CsrfToken {
  fn new(secret: Secret, rotation: Rotation) -> Self {
    Self {
      masked: Arc::new(Masked {
        secret,
        text: OnceLock::new(),
      }),
      rotation,
    }
  }

  #[cfg(test)]
  fn generate() -> Self {
    Self::new(random_secret(), Rotation::default())
  }

  /// The masked token text (base64url).
  pub fn as_str(&self) -> &str {
    let Masked { secret, text } = &*self.masked;
    text.get_or_init(|| mask(secret))
  }

  /// Issue a new secret for this session and return its token; the response sets it in the
  /// CSRF cookie, so tokens rendered before (this one included) stop validating for the client.
  /// Call it when the session's privilege changes, at login above all.
  pub fn rotate(&self) -> CsrfToken {
    let secret = random_secret();
    self.rotation.set(secret);
    Self::new(secret, self.rotation.clone())
  }
}

impl PartialEq for CsrfToken {
  fn eq(&self, other: &Self) -> bool {
    self.masked.secret.ct_eq(&other.masked.secret).into()
  }
}

impl Eq for CsrfToken {}

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

/// Whether the request carries `Authorization: Bearer …` and no `Cookie` header at all: nothing
/// a browser attaches on its own authenticates it, so another site cannot forge it. (`Basic`
/// and other schemes do not count: browsers resend cached credentials of those cross-site.)
fn is_bearer_only(headers: &HeaderMap) -> bool {
  !headers.contains_key(header::COOKIE)
    && headers
      .get(header::AUTHORIZATION)
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.get(..7))
      .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer "))
}

/// The CSRF part of [`SecurityLayer`](super::SecurityLayer).
#[derive(Clone, Debug)]
pub(crate) struct CsrfProtection {
  key: CookieKey,
  /// HMAC-SHA256 keyed with `key`'s signing half, as `cookie`'s signed jar signs.
  signer: Hmac<Sha256>,
  exempt: Vec<String>,
  exempt_bearer: bool,
  form_limit: usize,
  read_timeout: Duration,
}

/// The CSRF cookie to send once the response is known: a newly issued secret, unless the handler
/// rotated it ([`CsrfToken::rotate`]).
#[derive(Debug, Default)]
pub(crate) struct CookieUpdate {
  issued: Option<Secret>,
  rotation: Option<Rotation>,
}

impl CsrfProtection {
  pub(crate) fn new(key: CookieKey) -> Self {
    let signer = Hmac::new_from_slice(key.key().signing()).expect("HMAC takes any key length");
    Self {
      key,
      signer,
      exempt: Vec::new(),
      exempt_bearer: false,
      form_limit: DEFAULT_FORM_LIMIT,
      read_timeout: DEFAULT_READ_TIMEOUT,
    }
  }

  pub(crate) fn configure(&mut self, config: &CsrfConfig) {
    self.form_limit = config.form_limit;
    self.read_timeout = config.read_timeout;
  }

  pub(crate) fn exempt(&mut self, prefix: String) {
    let prefix = prefix.trim_end_matches('/').to_owned();
    self.exempt.push(prefix);
  }

  pub(crate) fn exempt_bearer(&mut self) {
    self.exempt_bearer = true;
  }

  fn is_exempt(&self, path: &str) -> bool {
    self.exempt.iter().any(|prefix| {
      prefix.is_empty()
        || path
          .strip_prefix(prefix.as_str())
          .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
  }

  /// The session secret from a correctly signed cookie, if any.
  fn session_secret(&self, headers: &HeaderMap) -> Option<Secret> {
    if !headers.contains_key(header::COOKIE) {
      return None;
    }
    let jar = request_jar(headers);
    let cookie = jar.signed(self.key.key()).get(CSRF_COOKIE)?;
    let secret = URL_SAFE_NO_PAD.decode(cookie.value()).ok()?;
    Secret::try_from(secret.as_slice()).ok()
  }

  /// The `Set-Cookie` value for `update`, if a secret was issued or rotated.
  pub(crate) fn cookie_header(&self, update: CookieUpdate) -> Option<HeaderValue> {
    let rotated = update.rotation.as_ref().and_then(Rotation::take);
    self.set_cookie(&rotated.or(update.issued)?)
  }

  /// `__Host-csrf=<signed secret>` and [`COOKIE_ATTRIBUTES`], signed the way `cookie`'s signed
  /// jar signs (`base64(HMAC-SHA256(value)) || value`, which [`Self::session_secret`] verifies)
  /// with the HMAC keyed once in [`Self::new`], and written in one buffer.
  fn set_cookie(&self, secret: &Secret) -> Option<HeaderValue> {
    let mut value = [0u8; SECRET_TEXT_LEN];
    let len = URL_SAFE_NO_PAD.encode_slice(secret, &mut value).ok()?;
    let value = &value[..len];
    let mut mac = self.signer.clone();
    mac.update(value);
    let tag = mac.finalize().into_bytes();
    let mut text = String::with_capacity(
      CSRF_COOKIE.len() + 1 + SIGNATURE_TEXT_LEN + len + COOKIE_ATTRIBUTES.len(),
    );
    text.push_str(CSRF_COOKIE);
    text.push('=');
    STANDARD.encode_string(tag, &mut text);
    text.push_str(std::str::from_utf8(value).ok()?);
    text.push_str(COOKIE_ATTRIBUTES);
    HeaderValue::from_maybe_shared(Bytes::from(text)).ok()
  }

  /// Resolve (or issue) the session secret, store its masked [`CsrfToken`] in the request
  /// extensions and check unsafe requests. Returns the request to run or the rejection, and the
  /// cookie update to resolve into `Set-Cookie` once the response is known
  /// ([`cookie_header`](Self::cookie_header)). A bearer-only request
  /// ([`SecurityLayer::csrf_exempt_bearer`](super::SecurityLayer::csrf_exempt_bearer)) skips all
  /// of it: no token, no cookie, no check.
  pub(crate) async fn guard(
    &self,
    mut request: Request,
  ) -> (Result<Request, Response>, CookieUpdate) {
    if self.exempt_bearer && is_bearer_only(request.headers()) {
      return (Ok(request), CookieUpdate::default());
    }
    let existing = self.session_secret(request.headers());
    let issued = existing.is_none();
    let secret = existing.unwrap_or_else(random_secret);
    let rotation = Rotation::default();
    let update = CookieUpdate {
      issued: issued.then_some(secret),
      rotation: Some(rotation.clone()),
    };
    request
      .extensions_mut()
      .insert(CsrfToken::new(secret, rotation));
    if is_safe(request.method()) || self.is_exempt(request.uri().path()) {
      return (Ok(request), update);
    }
    if issued {
      // No valid session cookie: nothing the client sends can match.
      return (Err(forbidden()), update);
    }
    match self.submitted_token(request).await {
      Ok((request, Some(submitted))) if masked_matches(&secret, &submitted) => {
        (Ok(request), update)
      }
      Ok(_) => (Err(forbidden()), update),
      Err(unread) => (Err(unread.into_response()), update),
    }
  }

  /// The token echoed in [`CSRF_HEADER`], else in a urlencoded form body's [`CSRF_FORM_FIELD`]
  /// (the body is buffered and put back). The body is read only up to `form_limit` bytes and
  /// within `read_timeout`; past either the request is rejected (413, 408).
  async fn submitted_token(&self, request: Request) -> Result<(Request, Option<String>), Unread> {
    if let Some(value) = request.headers().get(CSRF_HEADER) {
      let value = value.to_str().ok().map(str::to_owned);
      return Ok((request, value));
    }
    let is_form = request
      .headers()
      .get(header::CONTENT_TYPE)
      .and_then(|v| v.to_str().ok())
      .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
    if !is_form {
      return Ok((request, None));
    }
    let declared = request
      .headers()
      .get(header::CONTENT_LENGTH)
      .and_then(|v| v.to_str().ok())
      .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > self.form_limit as u64) {
      return Err(Unread::TooLarge);
    }
    let (parts, body) = request.into_parts();
    let read = axum::body::to_bytes(body, self.form_limit);
    match tokio::time::timeout(self.read_timeout, read).await {
      Ok(Ok(bytes)) => {
        let token = form_field(&bytes, CSRF_FORM_FIELD);
        Ok((Request::from_parts(parts, Body::from(bytes)), token))
      }
      Ok(Err(error)) if is_length_limit(&error) => Err(Unread::TooLarge),
      Ok(Err(_)) => Err(Unread::Broken),
      Err(_elapsed) => Err(Unread::TimedOut),
    }
  }
}

/// Why the form body could not be read for its token.
enum Unread {
  /// Over `form_limit` (declared or read): 413.
  TooLarge,
  /// Not complete within `read_timeout`: 408.
  TimedOut,
  /// The transport failed: 403, as for a missing token.
  Broken,
}

impl IntoResponse for Unread {
  fn into_response(self) -> Response {
    match self {
      Self::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "csrf form body too large").into_response(),
      Self::TimedOut => {
        (StatusCode::REQUEST_TIMEOUT, "csrf form body read timed out").into_response()
      }
      Self::Broken => forbidden(),
    }
  }
}

/// Whether a body read failed on the length limit (not on the transport).
fn is_length_limit(error: &axum::Error) -> bool {
  let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
  while let Some(error) = source {
    if error.is::<http_body_util::LengthLimitError>() {
      return true;
    }
    source = error.source();
  }
  false
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

/// Limits of the CSRF check, installed with
/// [`SecurityLayer::csrf_config`](super::SecurityLayer::csrf_config).
///
/// [`CsrfConfig::recommended`] (also `Default`) reads a urlencoded form body of at most 64 KiB
/// within 10 s to find [`CSRF_FORM_FIELD`]; a larger body is rejected with 413, a slower one
/// with 408. The deprecated [`CsrfConfig::new`] instead carries the 0.7 static token for
/// [`csrf_middleware`].
#[derive(Clone)]
pub struct CsrfConfig {
  token: Arc<str>,
  form_limit: usize,
  read_timeout: Duration,
}

impl Default for CsrfConfig {
  fn default() -> Self {
    Self::recommended()
  }
}

impl fmt::Debug for CsrfConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CsrfConfig")
      .field("form_limit", &self.form_limit)
      .field("read_timeout", &self.read_timeout)
      .finish_non_exhaustive()
  }
}

impl CsrfConfig {
  /// 64 KiB form limit, 10 s read timeout.
  pub fn recommended() -> Self {
    Self {
      token: Arc::from(""),
      form_limit: DEFAULT_FORM_LIMIT,
      read_timeout: DEFAULT_READ_TIMEOUT,
    }
  }

  /// Largest urlencoded form body read to find [`CSRF_FORM_FIELD`], in bytes.
  pub fn form_limit(mut self, bytes: usize) -> Self {
    self.form_limit = bytes;
    self
  }

  /// Time allowed to read that form body.
  pub fn read_timeout(mut self, timeout: Duration) -> Self {
    self.read_timeout = timeout;
    self
  }

  /// Use `token` for validation on POST/PUT/PATCH/DELETE.
  #[deprecated(
    since = "0.8.0",
    note = "a static shared token is not per-session; use `SecurityLayer::recommended(key)`"
  )]
  pub fn new(token: impl Into<Arc<str>>) -> Self {
    Self {
      token: token.into(),
      ..Self::recommended()
    }
  }

  /// Expected header value of the deprecated [`csrf_middleware`] (empty unless built with
  /// [`CsrfConfig::new`]; an empty token matches nothing).
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
      .is_some_and(|submitted| {
        !config.token().is_empty() && tokens_match(config.token(), submitted)
      });
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
  use cookie::{Cookie, CookieJar};
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
  fn mask_round_trips_with_a_fresh_pad() {
    let secret = random_secret();
    let (a, b) = (mask(&secret), mask(&secret));
    assert_ne!(a, b);
    assert_eq!(a.len(), 86);
    assert_eq!(unmask(&a).as_deref(), Some(&secret[..]));
    assert!(masked_matches(&secret, &b));
    assert!(!masked_matches(&random_secret(), &a));
    // The unmasked cookie form and malformed text never match.
    assert!(!masked_matches(
      &secret,
      &URL_SAFE_NO_PAD.encode(&secret[..])
    ));
    assert!(!masked_matches(&secret, "not base64!"));
  }

  #[test]
  fn set_cookie_matches_the_cookie_crate_and_verifies() {
    let key = CookieKey::generate();
    let csrf = CsrfProtection::new(key.clone());
    let secret = random_secret();
    let value = csrf.set_cookie(&secret).unwrap();
    // The header the `cookie` builder writes for the same cookie (signing is deterministic).
    let cookie = Cookie::build((CSRF_COOKIE, URL_SAFE_NO_PAD.encode(secret)))
      .path("/")
      .secure(true)
      .http_only(true)
      .same_site(cookie::SameSite::Strict)
      .build();
    let mut jar = CookieJar::new();
    jar.signed_mut(key.key()).add(cookie);
    assert_eq!(value, jar.get(CSRF_COOKIE).unwrap().to_string());
    let pair = value.to_str().unwrap().split(';').next().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::COOKIE, HeaderValue::from_str(pair).unwrap());
    assert_eq!(csrf.session_secret(&headers), Some(secret));
  }

  #[test]
  fn token_is_masked_once_and_shared_by_clones() {
    let token = CsrfToken::generate();
    let clone = token.clone();
    assert_eq!(token.as_str(), clone.as_str());
    assert!(masked_matches(&token.masked.secret, token.as_str()));
    let other = CsrfToken::new(token.masked.secret, Rotation::default());
    assert_eq!(other, token);
    assert_ne!(other.as_str(), token.as_str());
  }

  #[test]
  fn csrf_config_builders_set_limits() {
    let config = CsrfConfig::default()
      .form_limit(10)
      .read_timeout(Duration::from_millis(5));
    let mut csrf = CsrfProtection::new(CookieKey::generate());
    assert_eq!(csrf.form_limit, DEFAULT_FORM_LIMIT);
    csrf.configure(&config);
    assert_eq!(csrf.form_limit, 10);
    assert_eq!(csrf.read_timeout, Duration::from_millis(5));
    assert_eq!(config.token(), "");
    assert!(!format!("{config:?}").contains("token"));
  }

  #[tokio::test]
  async fn csrf_middleware_with_empty_token_matches_nothing() {
    let app = guarded(CsrfConfig::default());
    assert_eq!(status(app, "POST", Some("")).await, StatusCode::FORBIDDEN);
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

  /// A POST through `SecurityLayer::recommended` (bearer exemption on or off) with `headers`.
  async fn secured_post(exempt_bearer: bool, headers: &[(header::HeaderName, &str)]) -> Response {
    let mut layer = super::super::SecurityLayer::recommended(CookieKey::generate());
    if exempt_bearer {
      layer = layer.csrf_exempt_bearer();
    }
    let app = Router::new()
      .route("/", post(|| async { "ok" }))
      .layer(layer);
    let mut req = Request::builder().method("POST").uri("/");
    for (name, value) in headers {
      req = req.header(name, *value);
    }
    app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap()
  }

  #[test]
  fn bearer_only_needs_a_bearer_scheme_and_no_cookie() {
    let headers = |pairs: &[(header::HeaderName, &str)]| {
      let mut map = HeaderMap::new();
      for (name, value) in pairs {
        map.insert(name, HeaderValue::from_str(value).unwrap());
      }
      map
    };
    assert!(is_bearer_only(&headers(&[(
      header::AUTHORIZATION,
      "Bearer t"
    )])));
    assert!(is_bearer_only(&headers(&[(
      header::AUTHORIZATION,
      "bearer t"
    )])));
    assert!(!is_bearer_only(&headers(&[(
      header::AUTHORIZATION,
      "Basic dTpw"
    )])));
    assert!(!is_bearer_only(&headers(&[(
      header::AUTHORIZATION,
      "Bear"
    )])));
    assert!(!is_bearer_only(&HeaderMap::new()));
    assert!(!is_bearer_only(&headers(&[
      (header::AUTHORIZATION, "Bearer t"),
      (header::COOKIE, "session=s"),
    ])));
  }

  #[tokio::test]
  async fn csrf_exempt_bearer_lets_bearer_only_requests_through() {
    let bearer = [(header::AUTHORIZATION, "Bearer api-token")];
    // Off by default: the bearer request is checked like any other.
    assert_eq!(
      secured_post(false, &bearer).await.status(),
      StatusCode::FORBIDDEN
    );
    let ok = secured_post(true, &bearer).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert!(
      ok.headers().get(header::SET_COOKIE).is_none(),
      "no CSRF cookie for a bearer-only client"
    );
  }

  #[tokio::test]
  async fn csrf_exempt_bearer_still_checks_cookie_and_basic_requests() {
    for headers in [
      // A cookie may be what authenticates it: forgeable, so checked.
      vec![
        (header::AUTHORIZATION, "Bearer api-token"),
        (header::COOKIE, "session=s"),
      ],
      vec![(header::COOKIE, "session=s")],
      // Browsers resend cached Basic credentials cross-site.
      vec![(header::AUTHORIZATION, "Basic dTpw")],
      vec![],
    ] {
      assert_eq!(
        secured_post(true, &headers).await.status(),
        StatusCode::FORBIDDEN,
        "{headers:?}"
      );
    }
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
