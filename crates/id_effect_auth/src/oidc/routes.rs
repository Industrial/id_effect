//! [`OidcRoutes`]: the browser side of an OpenID Connect login as axum routes.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http::header::{CACHE_CONTROL, COOKIE, LOCATION, SET_COOKIE};
use http::{HeaderMap, HeaderValue, StatusCode, Uri};
use id_effect::{Cause, Effect, Exit, HNil};
use id_effect_authz::Principal as _;
use id_effect_axum::route::Problem;

use super::{OidcClient, OidcError, PendingLogin};
use crate::principal::{
  AuthPrincipal, SESSION_PERMISSIONS_ATTR, SESSION_ROLES_ATTR, SESSION_TENANT_ATTR,
};
use crate::session::{
  CookieConfig, MemorySessionStore, Session, SessionConfig, SessionId, Sessions,
};

/// Default path of the route that starts a login.
pub const DEFAULT_LOGIN_PATH: &str = "/auth/oidc/login";
/// Default path of the redirect URI (register `https://<host>/auth/oidc/callback`).
pub const DEFAULT_CALLBACK_PATH: &str = "/auth/oidc/callback";
/// Default path of the logout route (`POST`).
pub const DEFAULT_LOGOUT_PATH: &str = "/auth/oidc/logout";
/// Default cookie of the flow session that holds a pending login.
pub const DEFAULT_FLOW_COOKIE: &str = "__Host-id_oidc_flow";
/// How long a started login may take before its flow session expires.
pub const FLOW_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Session attribute holding the ID token of an OIDC login (the `id_token_hint` at logout).
pub const SESSION_ID_TOKEN_ATTR: &str = "oidc_id_token";

/// User id of flow sessions (they are never application sessions: their cookie is another one).
const FLOW_USER: &str = "oidc-login-in-progress";

/// The OpenID Connect login routes over an [`OidcClient`] and the application's [`Sessions`].
/// Mount [`router`](Self::router) with `Router::merge`.
///
/// | Route | Does |
/// |---|---|
/// | `GET` login path (`?return_to=/local/path` optional) | starts a flow session holding `state`, `nonce` and the PKCE verifier; **303** to the provider |
/// | `GET` callback path | consumes the flow session, checks `state` (and `iss` when sent), exchanges the code, verifies the ID token, logs in with [`Sessions::login`] (a fresh session id; the roles, permissions, tenant and ID token go into the session's attributes); **303** to `return_to` or [`after_login`](Self::after_login). Failures answer Problem Details (see [`OidcError::http_status`]). |
/// | `POST` logout path | ends the session; **303** to the provider's end-session endpoint for an OIDC session (RP-initiated logout), else to [`after_logout`](Self::after_logout) |
///
/// The flow session lives in its own store and cookie (default: in memory, cookie
/// [`DEFAULT_FLOW_COOKIE`], [`FLOW_TIMEOUT`]); give several instances a shared store with
/// [`flow_sessions`](Self::flow_sessions). Its cookie is `SameSite=Lax`, which browsers send on
/// the provider's top-level redirect back, and never authenticates a request.
#[derive(Clone)]
pub struct OidcRoutes {
  client: OidcClient,
  sessions: Sessions,
  flow: Sessions,
  login_path: String,
  callback_path: String,
  logout_path: String,
  after_login: String,
  after_logout: String,
  provider_logout: bool,
}

impl fmt::Debug for OidcRoutes {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OidcRoutes")
      .field("client", &self.client)
      .field("login_path", &self.login_path)
      .field("callback_path", &self.callback_path)
      .field("logout_path", &self.logout_path)
      .finish_non_exhaustive()
  }
}

impl OidcRoutes {
  /// Routes logging users of `client`'s provider into `sessions`, at the default paths.
  pub fn new(client: OidcClient, sessions: Sessions) -> Self {
    Self {
      client,
      sessions,
      flow: Sessions::new(MemorySessionStore::new(), Self::flow_session_config()),
      login_path: DEFAULT_LOGIN_PATH.into(),
      callback_path: DEFAULT_CALLBACK_PATH.into(),
      logout_path: DEFAULT_LOGOUT_PATH.into(),
      after_login: "/".into(),
      after_logout: "/".into(),
      provider_logout: true,
    }
  }

  /// The default flow-session configuration: [`FLOW_TIMEOUT`] and cookie
  /// [`DEFAULT_FLOW_COOKIE`] (`__Host-`, `Secure`, `HttpOnly`, `SameSite=Lax`).
  pub fn flow_session_config() -> SessionConfig {
    SessionConfig {
      idle_timeout: FLOW_TIMEOUT,
      absolute_timeout: FLOW_TIMEOUT,
      touch_interval: FLOW_TIMEOUT,
      cookie: CookieConfig {
        name: DEFAULT_FLOW_COOKIE.into(),
        ..CookieConfig::default()
      },
    }
  }

  /// Keep pending logins in `flow` (for example a `PgSessionStore` with
  /// [`flow_session_config`](Self::flow_session_config), shared by every instance). Its cookie
  /// name must differ from the application sessions' one.
  #[must_use]
  pub fn flow_sessions(mut self, flow: Sessions) -> Self {
    self.flow = flow;
    self
  }

  /// Serve the login route at `path` (default [`DEFAULT_LOGIN_PATH`]).
  #[must_use]
  pub fn login_path(mut self, path: impl Into<String>) -> Self {
    self.login_path = path.into();
    self
  }

  /// Serve the callback at `path` (default [`DEFAULT_CALLBACK_PATH`]); it must be the path of
  /// the configured redirect URL.
  #[must_use]
  pub fn callback_path(mut self, path: impl Into<String>) -> Self {
    self.callback_path = path.into();
    self
  }

  /// Serve the logout route at `path` (default [`DEFAULT_LOGOUT_PATH`]).
  #[must_use]
  pub fn logout_path(mut self, path: impl Into<String>) -> Self {
    self.logout_path = path.into();
    self
  }

  /// Where a login without `return_to` lands (default `/`).
  #[must_use]
  pub fn after_login(mut self, location: impl Into<String>) -> Self {
    self.after_login = location.into();
    self
  }

  /// Where logout lands when it does not continue to the provider (default `/`).
  #[must_use]
  pub fn after_logout(mut self, location: impl Into<String>) -> Self {
    self.after_logout = location.into();
    self
  }

  /// Whether logout continues to the provider's end-session endpoint (default `true`).
  #[must_use]
  pub fn provider_logout(mut self, enabled: bool) -> Self {
    self.provider_logout = enabled;
    self
  }

  /// The three routes, for `Router::merge` into the application's router.
  pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
    let (login_path, callback_path, logout_path) = (
      self.login_path.clone(),
      self.callback_path.clone(),
      self.logout_path.clone(),
    );
    Router::new()
      .route(&login_path, get(login))
      .route(&callback_path, get(callback))
      .route(&logout_path, post(logout))
      .with_state(Arc::new(self))
  }
}

type Routes = State<Arc<OidcRoutes>>;

async fn login(State(r): Routes, uri: Uri, headers: HeaderMap) -> Response {
  let params = query(&uri);
  let mut request = r.client.begin();
  if let Some(path) = params.get("return_to").filter(|p| is_local_path(p)) {
    request.pending = request.pending.with_return_to(path.as_str());
  }
  let presented = cookie_id(&r.flow, &headers);
  let (flow, pending) = (r.flow.clone(), request.pending);
  let started = r
    .flow
    .login(FLOW_USER, presented.as_ref())
    .flat_map(move |mut session| {
      pending.store(&mut session);
      flow.save(session.clone()).map(move |()| session)
    })
    .map_err(OidcError::Session);
  match exec(started).await {
    Ok(session) => {
      let mut response = see_other(&request.url);
      append_cookie(&mut response, &r.flow.set_cookie(&session));
      response
    }
    Err(e) => problem(&e),
  }
}

async fn callback(State(r): Routes, uri: Uri, headers: HeaderMap) -> Response {
  let flow_id = cookie_id(&r.flow, &headers);
  let presented = cookie_id(&r.sessions, &headers);
  let mut response = match complete(&r, flow_id, presented, &query(&uri)).await {
    Ok((session, location)) => {
      let mut response = see_other(&location);
      append_cookie(&mut response, &r.sessions.set_cookie(&session));
      response
    }
    Err(e) => problem(&e),
  };
  append_cookie(&mut response, &r.flow.clear_cookie());
  response
}

/// The callback's checks and the login; the application session and where to go.
async fn complete(
  r: &OidcRoutes,
  flow_id: Option<SessionId>,
  presented: Option<SessionId>,
  params: &HashMap<String, String>,
) -> Result<(Session, String), OidcError> {
  let no_login = || OidcError::InvalidCallback("no login is in progress in this browser".into());
  let flow_id = flow_id.ok_or_else(no_login)?;
  // Consumed before anything else: a callback URL works once.
  let flow = r.flow.clone();
  let consumed = r
    .flow
    .load(&flow_id)
    .flat_map(move |found| flow.logout(&flow_id).map(move |_| found));
  let pending = exec(consumed.map_err(OidcError::Session))
    .await?
    .as_ref()
    .and_then(PendingLogin::from_session)
    .ok_or_else(no_login)?;
  let param = |name: &str| params.get(name).map(String::as_str);
  if !param("state").is_some_and(|s| pending.state_matches(s)) {
    return Err(OidcError::InvalidCallback(
      "state does not match the login".into(),
    ));
  }
  if let Some(error) = param("error") {
    return Err(OidcError::Denied {
      error: error.to_owned(),
      description: param("error_description").map(str::to_owned),
    });
  }
  // RFC 9207: a provider that names itself must be the one the login started with.
  if param("iss").is_some_and(|iss| iss != r.client.issuer()) {
    return Err(OidcError::InvalidCallback("iss is not the provider".into()));
  }
  let code = param("code").ok_or_else(|| OidcError::InvalidCallback("no code".into()))?;
  let done = exec(r.client.exchange(code, &pending)).await?;
  let sessions = r.sessions.clone();
  let user = done.principal.id().to_owned();
  let session = r
    .sessions
    .login(user, presented.as_ref())
    .flat_map(move |mut session| {
      record(&mut session, &done.principal, &done.id_token);
      sessions.save(session.clone()).map(move |()| session)
    })
    .map_err(OidcError::Session);
  let session = exec(session).await?;
  let location = pending.return_to.unwrap_or_else(|| r.after_login.clone());
  Ok((session, location))
}

/// The principal's roles, permissions and tenant (read back by
/// [`AuthPrincipal::from_session`]) and the ID token, as session attributes.
fn record(session: &mut Session, principal: &AuthPrincipal, id_token: &str) {
  if !principal.roles().is_empty() {
    session.set_attr(SESSION_ROLES_ATTR, principal.roles().join(" "));
  }
  if !principal.permissions().is_empty() {
    session.set_attr(SESSION_PERMISSIONS_ATTR, principal.permissions().join(" "));
  }
  if let Some(tenant) = principal.tenant() {
    session.set_attr(SESSION_TENANT_ATTR, tenant);
  }
  session.set_attr(SESSION_ID_TOKEN_ATTR, id_token);
}

async fn logout(State(r): Routes, headers: HeaderMap) -> Response {
  let mut id_token = None;
  if let Some(id) = cookie_id(&r.sessions, &headers) {
    let sessions = r.sessions.clone();
    let ended = r
      .sessions
      .load(&id)
      .flat_map(move |found| sessions.logout(&id).map(move |_| found))
      .map_err(OidcError::Session);
    match exec(ended).await {
      Ok(found) => {
        id_token = found.and_then(|s| s.attr(SESSION_ID_TOKEN_ATTR).map(str::to_owned));
      }
      Err(e) => return problem(&e),
    }
  }
  let provider = id_token
    .filter(|_| r.provider_logout)
    .and_then(|hint| r.client.logout_url(Some(&hint)));
  let mut response = see_other(provider.as_deref().unwrap_or(&r.after_logout));
  append_cookie(&mut response, &r.sessions.clear_cookie());
  response
}

/// Runs a login step on the request's fiber.
async fn exec<A: Send + 'static>(eff: Effect<A, OidcError>) -> Result<A, OidcError> {
  match id_effect_axum::run_with_env(HNil, move |_| eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(other) => Err(OidcError::Provider(format!(
      "login step ended abnormally: {other:?}"
    ))),
  }
}

/// The query parameters (the first of repeated names).
fn query(uri: &Uri) -> HashMap<String, String> {
  let mut params = HashMap::new();
  let pairs = openidconnect::url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes());
  for (name, value) in pairs {
    params
      .entry(name.into_owned())
      .or_insert_with(|| value.into_owned());
  }
  params
}

/// A same-origin path (`/x`, not `//host` or `/\host`), so `return_to` cannot redirect off-site.
fn is_local_path(path: &str) -> bool {
  path.starts_with('/')
    && !path.starts_with("//")
    && !path.starts_with("/\\")
    && path.chars().all(|c| c.is_ascii_graphic())
}

fn cookie_id(sessions: &Sessions, headers: &HeaderMap) -> Option<SessionId> {
  headers
    .get_all(COOKIE)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .find_map(|h| sessions.session_id(h))
}

fn no_store(response: &mut Response) {
  response
    .headers_mut()
    .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

fn see_other(location: &str) -> Response {
  let mut response = StatusCode::SEE_OTHER.into_response();
  let location = HeaderValue::from_str(location).unwrap_or(HeaderValue::from_static("/"));
  response.headers_mut().insert(LOCATION, location);
  no_store(&mut response);
  response
}

fn append_cookie(response: &mut Response, cookie: &str) {
  if let Ok(value) = HeaderValue::from_str(cookie) {
    response.headers_mut().append(SET_COOKIE, value);
  }
}

/// Problem Details for `e`; server-side causes are not described to the client. The error is a
/// response extension (for logging middleware).
fn problem(e: &OidcError) -> Response {
  let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
  let detail = if status.is_server_error() {
    "the login could not be completed; try again later".to_owned()
  } else {
    e.to_string()
  };
  let mut response = Problem::new(status).with_detail(detail).into_response();
  no_store(&mut response);
  response.extensions_mut().insert(e.clone());
  response
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_local_paths_are_return_targets() {
    for ok in ["/", "/ledger", "/a/b?c=d#e"] {
      assert!(is_local_path(ok), "{ok}");
    }
    for bad in [
      "",
      "ledger",
      "//evil.example",
      "/\\evil.example",
      "https://evil",
      "/a b",
      "/é",
    ] {
      assert!(!is_local_path(bad), "{bad}");
    }
  }

  #[test]
  fn query_keeps_the_first_of_repeated_names() {
    let uri: Uri = "/cb?state=a&state=b&code=c%20d".parse().unwrap();
    let q = query(&uri);
    assert_eq!(q.get("state").map(String::as_str), Some("a"));
    assert_eq!(q.get("code").map(String::as_str), Some("c d"));
    assert!(query(&"/cb".parse().unwrap()).is_empty());
  }

  #[test]
  fn responses() {
    let r = see_other("/home");
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(r.headers()[LOCATION], "/home");
    assert_eq!(r.headers()[CACHE_CONTROL], "no-store");
    assert_eq!(see_other("/bad\n").headers()[LOCATION], "/");
    let mut r = see_other("/");
    append_cookie(&mut r, "a=b");
    append_cookie(&mut r, "bad\ncookie");
    assert_eq!(r.headers().get_all(SET_COOKIE).iter().count(), 1);

    let e = OidcError::Discovery("secret detail".into());
    let r = problem(&e);
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.extensions().get::<OidcError>(), Some(&e));
    let r = problem(&OidcError::InvalidCallback("state".into()));
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
  }

  #[test]
  fn flow_config_uses_its_own_cookie() {
    let c = OidcRoutes::flow_session_config();
    assert_eq!(c.cookie.name, DEFAULT_FLOW_COOKIE);
    assert_eq!(c.absolute_timeout, FLOW_TIMEOUT);
    assert_ne!(c.cookie.name, SessionConfig::default().cookie.name);
  }
}
