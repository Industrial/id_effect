//! [`Authenticate`] (feature `axum`): declarative credential middleware that turns a request's
//! session cookie, bearer JWT, API key or Basic credentials into its principal.
//!
//! Configure the schemes a service accepts; the layer tries them in order (by default
//! **session → bearer → API key → Basic**, change it with [`Authenticate::order`]) and stops at
//! the first credential the request carries:
//!
//! | Request | Outcome |
//! |---|---|
//! | no credential of a configured scheme (or an expired session) | continues as [`Anonymous`](id_effect_authz::Anonymous): it reaches **public** typed routes only; a guarded typed route answers 401 (see below) |
//! | a valid credential | continues as that [`AuthPrincipal`] |
//! | an invalid credential (bad signature, expired token, wrong password, unknown key) | **401** Problem Details with `WWW-Authenticate` challenges, at once |
//! | a backend failure (user store, JWKS endpoint) | **503** Problem Details |
//! | Basic credentials for an account or client address the [`LoginThrottle`](crate::LoginThrottle) holds back | **429** Problem Details with `Retry-After` |
//!
//! Basic credentials are throttled per account and per client address; the address is the
//! peer's (axum's `ConnectInfo<SocketAddr>`, present when the server was started with
//! `into_make_service_with_connect_info`).
//!
//! The principal is installed the way `id_effect_authz`'s guard expects it: a
//! [`CurrentPrincipal`] request extension, the same value in [`RequestCaps`] (so effect handlers
//! run with `execute_with_request_caps` can `need::<CurrentPrincipal>()`), and the
//! [`AuthPrincipal`] itself as an extension (`Extension<AuthPrincipal>`). Add `Authenticate`
//! with `Router::layer`, outside the per-route guards.
//!
//! Continuing as `Anonymous` does not open anything: every typed route declares its access
//! (`id_effect_axum::route::Route::public` or `Route::guard`), a typed route's guard answers the
//! anonymous principal **401** before its policy runs, and a route that declares neither does not
//! compile. A request without credentials therefore reaches only the routes declared public.
//!
//! ```
//! # #[cfg(feature = "jwt")] {
//! use axum::{Router, routing::get};
//! use id_effect_auth::{ApiKeys, Authenticate, JwtKey, JwtVerifier, MemoryApiKeyStore};
//! use id_effect_authz::guard::{AuthorizeExt, Guard};
//! use id_effect_authz::authenticated;
//!
//! let jwt = JwtVerifier::new([JwtKey::hmac(b"a 256-bit secret for HS256 tokens")])
//!   .issuer("https://id.example.com")
//!   .audience("ledger");
//! let keys = ApiKeys::new(MemoryApiKeyStore::new(), "ledger");
//!
//! let app: Router = Router::new()
//!   .route("/me", get(|| async { "me" }).authorize(Guard::new("me", authenticated())))
//!   .layer(Authenticate::bearer_jwt(jwt).with_api_key(keys).realm("ledger"));
//! # let _ = app;
//! # }
//! ```

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::ConnectInfo;
use axum::response::{IntoResponse, Response};
use http::header::{AUTHORIZATION, COOKIE, RETRY_AFTER, WWW_AUTHENTICATE};
use http::request::Parts;
use http::{HeaderName, HeaderValue, Request, StatusCode};
use id_effect::{Cause, Effect, Exit, HNil};
use id_effect_authz::CurrentPrincipal;
use id_effect_axum::RequestCaps;
use id_effect_axum::route::Problem;
use serde_json::json;
use tower_layer::Layer;
use tower_service::Service;

use crate::apikey::ApiKeys;
use crate::basic::BasicAuth;
use crate::error::{AuthError, retry_after_secs};
#[cfg(feature = "jwt")]
use crate::jwt::JwtVerifier;
use crate::principal::{AuthPrincipal, AuthScheme};
use crate::session::{Session, Sessions};

/// Default API-key header.
pub const DEFAULT_API_KEY_HEADER: &str = "x-api-key";

/// Default realm in `WWW-Authenticate` challenges.
pub const DEFAULT_REALM: &str = "api";

/// The default order schemes are tried in.
pub const DEFAULT_ORDER: [AuthScheme; 4] = [
  AuthScheme::Session,
  AuthScheme::Bearer,
  AuthScheme::ApiKey,
  AuthScheme::Basic,
];

/// Credential middleware: a Tower [`Layer`] that authenticates each request with the configured
/// schemes. Cheap to clone. See the [module documentation](self).
#[derive(Clone)]
pub struct Authenticate {
  sessions: Option<Sessions>,
  #[cfg(feature = "jwt")]
  bearer: Option<JwtVerifier>,
  api_keys: Option<ApiKeys>,
  basic: Option<BasicAuth>,
  order: Arc<[AuthScheme]>,
  realm: Arc<str>,
  api_key_header: HeaderName,
}

impl Default for Authenticate {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Debug for Authenticate {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Authenticate")
      .field("schemes", &self.schemes())
      .field("realm", &self.realm)
      .field("api_key_header", &self.api_key_header)
      .finish()
  }
}

impl Authenticate {
  /// No scheme configured: every request continues anonymously until schemes are added.
  pub fn new() -> Self {
    Self {
      sessions: None,
      #[cfg(feature = "jwt")]
      bearer: None,
      api_keys: None,
      basic: None,
      order: Arc::new(DEFAULT_ORDER),
      realm: Arc::from(DEFAULT_REALM),
      api_key_header: HeaderName::from_static(DEFAULT_API_KEY_HEADER),
    }
  }

  /// Authenticate with server-side sessions only.
  pub fn session(sessions: Sessions) -> Self {
    Self::new().with_session(sessions)
  }

  /// Authenticate with `Authorization: Bearer <JWT>` only.
  #[cfg(feature = "jwt")]
  pub fn bearer_jwt(verifier: JwtVerifier) -> Self {
    Self::new().with_bearer_jwt(verifier)
  }

  /// Authenticate with API keys only.
  pub fn api_key(keys: ApiKeys) -> Self {
    Self::new().with_api_key(keys)
  }

  /// Authenticate with `Authorization: Basic` only.
  pub fn basic(basic: BasicAuth) -> Self {
    Self::new().with_basic(basic)
  }

  /// Also accept the session cookie of `sessions` (a `Session` already loaded by an outer
  /// `SessionLayer` is used as is). A cookie naming no live session counts as no credential.
  /// The principal is [`AuthPrincipal::from_session`].
  #[must_use]
  pub fn with_session(mut self, sessions: Sessions) -> Self {
    self.sessions = Some(sessions);
    self
  }

  /// Also accept bearer JWTs checked by `verifier`.
  #[cfg(feature = "jwt")]
  #[must_use]
  pub fn with_bearer_jwt(mut self, verifier: JwtVerifier) -> Self {
    self.bearer = Some(verifier);
    self
  }

  /// Also accept API keys (in the [`api_key_header`](Self::api_key_header)) checked by `keys`.
  #[must_use]
  pub fn with_api_key(mut self, keys: ApiKeys) -> Self {
    self.api_keys = Some(keys);
    self
  }

  /// Also accept Basic credentials checked by `basic`.
  #[must_use]
  pub fn with_basic(mut self, basic: BasicAuth) -> Self {
    self.basic = Some(basic);
    self
  }

  /// Try the schemes in this order (schemes left out are not tried).
  #[must_use]
  pub fn order(mut self, order: impl IntoIterator<Item = AuthScheme>) -> Self {
    self.order = order.into_iter().collect();
    self
  }

  /// The realm named in `WWW-Authenticate` challenges (default [`DEFAULT_REALM`]).
  #[must_use]
  pub fn realm(mut self, realm: impl Into<String>) -> Self {
    self.realm = Arc::from(realm.into());
    self
  }

  /// Read API keys from header `name` (default [`DEFAULT_API_KEY_HEADER`]).
  #[must_use]
  pub fn api_key_header(mut self, name: HeaderName) -> Self {
    self.api_key_header = name;
    self
  }

  /// The configured schemes, in the order they are tried.
  pub fn schemes(&self) -> Vec<AuthScheme> {
    self
      .order
      .iter()
      .copied()
      .filter(|s| self.configured(*s))
      .collect()
  }

  fn configured(&self, scheme: AuthScheme) -> bool {
    match scheme {
      AuthScheme::Session => self.sessions.is_some(),
      #[cfg(feature = "jwt")]
      AuthScheme::Bearer => self.bearer.is_some(),
      #[cfg(not(feature = "jwt"))]
      AuthScheme::Bearer => false,
      AuthScheme::ApiKey => self.api_keys.is_some(),
      AuthScheme::Basic => self.basic.is_some(),
    }
  }

  /// Authenticate the request head `parts`: `Ok(None)` when it carries no credential of a
  /// configured scheme, `Ok(Some(principal))` for a valid one, `Err((scheme, error))` for an
  /// invalid one or a backend failure. A session loaded here is also recorded in `parts` (as
  /// `SessionLayer` would).
  pub async fn authenticate(
    &self,
    parts: &mut Parts,
  ) -> Result<Option<AuthPrincipal>, (AuthScheme, AuthError)> {
    for scheme in self.schemes() {
      let check = match scheme {
        AuthScheme::Session => {
          if let Some(principal) = self.session_principal(parts).await {
            return Ok(Some(principal));
          }
          None
        }
        AuthScheme::Bearer => self.bearer(parts),
        AuthScheme::ApiKey => self.api_key_check(parts),
        AuthScheme::Basic => self.basic_check(parts),
      };
      if let Some(check) = check {
        return run(check).await.map(Some).map_err(|e| (scheme, e));
      }
    }
    Ok(None)
  }

  /// The session principal, if the request has a live session (counts as "no credential"
  /// otherwise, including during a store outage, like `SessionLayer`).
  async fn session_principal(&self, parts: &mut Parts) -> Option<AuthPrincipal> {
    if let Some(session) = parts.extensions.get::<Session>() {
      return Some(AuthPrincipal::from_session(session));
    }
    let sessions = self.sessions.clone()?;
    let id = parts
      .headers
      .get_all(COOKIE)
      .iter()
      .filter_map(|v| v.to_str().ok())
      .find_map(|h| sessions.session_id(h))?;
    let Exit::Success(Some(session)) =
      id_effect_axum::run_with_env(HNil, move |_| sessions.load(&id)).await
    else {
      return None;
    };
    let principal = AuthPrincipal::from_session(&session);
    let mut caps = parts.extensions.remove::<RequestCaps>().unwrap_or_default();
    caps.insert(session.clone());
    parts.extensions.insert(caps);
    parts.extensions.insert(session);
    Some(principal)
  }

  #[cfg(feature = "jwt")]
  fn bearer(&self, parts: &Parts) -> Option<Effect<AuthPrincipal, AuthError>> {
    let verifier = self.bearer.as_ref()?;
    let token = authorization(parts, "bearer")?;
    Some(verifier.verify(token))
  }

  #[cfg(not(feature = "jwt"))]
  fn bearer(&self, _parts: &Parts) -> Option<Effect<AuthPrincipal, AuthError>> {
    None
  }

  fn api_key_check(&self, parts: &Parts) -> Option<Effect<AuthPrincipal, AuthError>> {
    let keys = self.api_keys.as_ref()?;
    let value = parts.headers.get(&self.api_key_header)?;
    Some(match value.to_str() {
      Ok(key) => keys.verify(key.trim()),
      Err(_) => id_effect::fail(AuthError::Malformed("API key is not ASCII".into())),
    })
  }

  fn basic_check(&self, parts: &Parts) -> Option<Effect<AuthPrincipal, AuthError>> {
    let basic = self.basic.as_ref()?;
    let encoded = authorization(parts, "basic")?;
    let client = parts
      .extensions
      .get::<ConnectInfo<SocketAddr>>()
      .map(|ConnectInfo(peer)| peer.ip());
    Some(basic.authenticate_encoded_from(encoded, client))
  }

  /// The `WWW-Authenticate` challenges of the configured schemes; the one for `failed` carries
  /// the error (RFC 6750 `error="invalid_token"` for bearer tokens).
  pub fn challenges(&self, failed: Option<(AuthScheme, &AuthError)>) -> Vec<String> {
    let realm = quote(&self.realm);
    self
      .schemes()
      .into_iter()
      .filter_map(|scheme| {
        let error = failed.and_then(|(s, e)| (s == scheme).then_some(e));
        match scheme {
          AuthScheme::Session => None,
          AuthScheme::Bearer => Some(match error {
            Some(e) => format!(
              "Bearer realm={realm}, error=\"invalid_token\", error_description={}",
              quote(&e.to_string())
            ),
            None => format!("Bearer realm={realm}"),
          }),
          AuthScheme::ApiKey => Some(format!(
            "ApiKey realm={realm}, header={}",
            quote(self.api_key_header.as_str())
          )),
          AuthScheme::Basic => Some(format!("Basic realm={realm}, charset=\"UTF-8\"")),
        }
      })
      .collect()
  }

  /// The response for a credential of `scheme` rejected with `error`: Problem Details with the
  /// error's status, `WWW-Authenticate` challenges on a 401, `Retry-After` on a 429, and the
  /// [`AuthError`] as a response extension (for logging middleware).
  pub fn reject(&self, scheme: AuthScheme, error: AuthError) -> Response {
    let status =
      StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let detail = match &error {
      AuthError::Unavailable(_) => "authentication is temporarily unavailable".to_owned(),
      AuthError::Key(_) | AuthError::Config(_) => "authentication is misconfigured".to_owned(),
      e => e.to_string(),
    };
    let mut response = Problem::new(status)
      .with_detail(detail)
      .with_extension("scheme", json!(scheme.as_str()))
      .into_response();
    if status == StatusCode::UNAUTHORIZED {
      for challenge in self.challenges(Some((scheme, &error))) {
        if let Ok(value) = HeaderValue::from_str(&challenge) {
          response.headers_mut().append(WWW_AUTHENTICATE, value);
        }
      }
    }
    if let AuthError::TooManyAttempts { retry_after } = &error {
      response.headers_mut().insert(
        RETRY_AFTER,
        HeaderValue::from(retry_after_secs(*retry_after)),
      );
    }
    response.extensions_mut().insert(error);
    response
  }
}

/// `"…"` with `"` and `\` (and control characters) replaced, for a challenge parameter.
fn quote(s: &str) -> String {
  let cleaned: String = s
    .chars()
    .map(|c| match c {
      '"' | '\\' => '\'',
      c if c.is_control() || !c.is_ascii() => '?',
      c => c,
    })
    .collect();
  format!("\"{cleaned}\"")
}

/// The credentials after `scheme` in the request's `Authorization` header (scheme names are
/// case-insensitive), if it uses that scheme.
fn authorization<'p>(parts: &'p Parts, scheme: &str) -> Option<&'p str> {
  let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
  let (name, credentials) = value.split_once(' ')?;
  name
    .eq_ignore_ascii_case(scheme)
    .then(|| credentials.trim())
    .filter(|c| !c.is_empty())
}

/// Runs a credential check on the request's fiber.
async fn run(check: Effect<AuthPrincipal, AuthError>) -> Result<AuthPrincipal, AuthError> {
  match id_effect_axum::run_with_env(HNil, move |_| check).await {
    Exit::Success(p) => Ok(p),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(other) => Err(AuthError::Unavailable(format!(
      "credential check ended abnormally: {other:?}"
    ))),
  }
}

/// Records `principal` as the request's principal (extension and request capability).
fn install(parts: &mut Parts, principal: CurrentPrincipal) {
  let mut caps = parts.extensions.remove::<RequestCaps>().unwrap_or_default();
  caps.insert(principal.clone());
  parts.extensions.insert(caps);
  parts.extensions.insert(principal);
}

/// `true` when something (an outer layer) already installed a principal.
fn has_principal(parts: &Parts) -> bool {
  parts.extensions.get::<CurrentPrincipal>().is_some()
    || parts
      .extensions
      .get::<RequestCaps>()
      .is_some_and(|caps| caps.get::<CurrentPrincipal>().is_some())
}

impl<Svc> Layer<Svc> for Authenticate {
  type Service = AuthenticateService<Svc>;

  fn layer(&self, inner: Svc) -> Self::Service {
    AuthenticateService {
      inner,
      auth: self.clone(),
    }
  }
}

/// Service produced by [`Authenticate`].
#[derive(Clone)]
pub struct AuthenticateService<Svc> {
  inner: Svc,
  auth: Authenticate,
}

impl<Svc> fmt::Debug for AuthenticateService<Svc> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AuthenticateService")
      .field("auth", &self.auth)
      .finish_non_exhaustive()
  }
}

impl<Svc, B> Service<Request<B>> for AuthenticateService<Svc>
where
  Svc: Service<Request<B>, Response = Response> + Clone + Send + 'static,
  Svc::Future: Send + 'static,
  B: Send + 'static,
{
  type Response = Response;
  type Error = Svc::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Svc::Error>> + Send>>;

  #[inline]
  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    // The clone that was polled ready serves this request (the usual Tower hand-off).
    let ready = self.inner.clone();
    let mut inner = std::mem::replace(&mut self.inner, ready);
    let auth = self.auth.clone();
    let (mut parts, body) = req.into_parts();
    Box::pin(async move {
      match auth.authenticate(&mut parts).await {
        Ok(Some(principal)) => {
          install(&mut parts, CurrentPrincipal::new(principal.clone()));
          parts.extensions.insert(principal);
        }
        Ok(None) if !has_principal(&parts) => install(&mut parts, CurrentPrincipal::anonymous()),
        Ok(None) => {}
        Err((scheme, error)) => return Ok(auth.reject(scheme, error)),
      }
      inner.call(Request::from_parts(parts, body)).await
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn head(headers: &[(&str, &str)]) -> Parts {
    let mut req = Request::get("/").body(()).unwrap();
    for (k, v) in headers {
      req.headers_mut().insert(
        HeaderName::from_bytes(k.as_bytes()).unwrap(),
        HeaderValue::from_str(v).unwrap(),
      );
    }
    req.into_parts().0
  }

  #[test]
  fn authorization_scheme_is_case_insensitive_and_needs_credentials() {
    let p = head(&[("authorization", "bEaReR  tok ")]);
    assert_eq!(authorization(&p, "bearer"), Some("tok"));
    assert_eq!(authorization(&p, "basic"), None);
    assert_eq!(
      authorization(&head(&[("authorization", "Bearer ")]), "bearer"),
      None
    );
    assert_eq!(
      authorization(&head(&[("authorization", "Bearer")]), "bearer"),
      None
    );
    assert_eq!(authorization(&head(&[]), "bearer"), None);
  }

  #[test]
  fn quoting_strips_what_cannot_be_in_a_quoted_string() {
    assert_eq!(quote(r#"a"b\c"#), "\"a'b'c\"");
    assert_eq!(quote("kid é\n"), "\"kid ??\"");
  }

  #[test]
  fn configuration_and_order() {
    let auth = Authenticate::default();
    assert!(auth.schemes().is_empty());
    assert!(auth.challenges(None).is_empty());
    let keys = ApiKeys::new(crate::apikey::MemoryApiKeyStore::new(), "k");
    let auth = Authenticate::api_key(keys)
      .order([AuthScheme::Basic, AuthScheme::ApiKey])
      .realm("r")
      .api_key_header(HeaderName::from_static("x-key"));
    assert_eq!(auth.schemes(), [AuthScheme::ApiKey]);
    assert_eq!(
      auth.challenges(None),
      ["ApiKey realm=\"r\", header=\"x-key\""]
    );
    let dbg = format!("{auth:?}");
    assert!(dbg.contains("ApiKey") && dbg.contains("x-key"), "{dbg}");
  }

  #[test]
  fn backend_failures_are_503_and_key_errors_500_without_challenges() {
    let auth = Authenticate::new();
    let res = auth.reject(AuthScheme::ApiKey, AuthError::Unavailable("db".into()));
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(res.headers().get(WWW_AUTHENTICATE).is_none());
    assert!(res.extensions().get::<AuthError>().is_some());
    let res = auth.reject(AuthScheme::Bearer, AuthError::Key("pem".into()));
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
  }

  #[tokio::test]
  async fn abnormal_checks_are_unavailable() {
    let died = run(id_effect::die("boom")).await;
    assert!(matches!(died, Err(AuthError::Unavailable(m)) if m.contains("abnormally")));
  }
}
