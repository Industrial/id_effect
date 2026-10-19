//! OpenID Connect login (feature `oidc`): [`OidcClient`] speaks to the identity provider,
//! [`OidcRoutes`] mounts the browser flow on an axum router.
//!
//! The flow is the authorization-code flow with PKCE (RFC 7636, `S256`) that OpenID Connect
//! Core and the OAuth 2.0 Security BCP (RFC 9700) recommend for every client:
//!
//! 1. [`OidcClient::discover`] reads the provider's `/.well-known/openid-configuration` (the
//!    `issuer` it names must be the configured one) and points a [`JwksClient`] at its key set.
//! 2. [`OidcClient::begin`] builds the authorization URL with a fresh `state`, `nonce` and PKCE
//!    challenge; the [`PendingLogin`] (state, nonce, code verifier) stays on the server.
//! 3. [`OidcClient::exchange`] trades the code for tokens, sending the code verifier, and
//!    verifies the ID token: signature over the provider's JWKS (rotated keys are picked up),
//!    `iss`, `aud` = the client id, `exp` with leeway, `azp`, and `nonce` = the pending one. Its
//!    claims become an [`AuthPrincipal`] through a [`ClaimMapping`].
//!
//! [`OidcRoutes`] runs those steps behind `GET /auth/oidc/login`, `GET /auth/oidc/callback` and
//! `POST /auth/oidc/logout` (paths configurable). The pending login is kept in a short-lived
//! server-side *flow session* under its own cookie, so a browser that has not logged in yet
//! never holds an application session; the callback consumes it (one use), checks `state` in
//! constant time, and logs in with [`Sessions::login`](crate::Sessions::login), which mints a
//! fresh session id. Logout ends the session and, when the provider publishes an
//! `end_session_endpoint`, continues to it (RP-initiated logout).
//!
//! [`OidcClient`] also implements id_effect_platform's [`OAuthClient`] for code written against
//! that capability; that trait has no place for a code verifier or a nonce, so prefer
//! [`begin`](OidcClient::begin) / [`exchange`](OidcClient::exchange) for browser logins.
//!
//! ```no_run
//! use id_effect_auth::oidc::{OidcClient, OidcConfig};
//! use id_effect_auth::ClaimMapping;
//!
//! let config = OidcConfig::new(
//!   "https://id.example.com/realms/ledger",
//!   "ledger-web",
//!   "https://ledger.example.com/auth/oidc/callback",
//! )
//! .client_secret("from the secret store")
//! .scopes(["profile", "email"])
//! .claims(ClaimMapping::default().roles(["realm_access.roles"]));
//! let client = OidcClient::discover(config); // an Effect; run it once at start-up
//! # let _ = client;
//! ```
//!
//! Requests go through reqwest with rustls and need a tokio runtime. The provider decides which
//! tokens are trusted, so they are strict (0.11 M3, the [`JwksClient`] rules): the issuer and the
//! token, user-info and JWKS endpoints it publishes must be `https` (plain `http` only to a
//! loopback host, for tests, or after [`OidcConfig::insecure_allow_http`]), redirects are not
//! followed (a `3xx` from discovery or the token endpoint fails the call), and every request has
//! a connect timeout ([`DEFAULT_CONNECT_TIMEOUT`]) and a total one ([`OidcConfig::timeout`]).

mod routes;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, WallClock, fail, from_async, succeed};
use id_effect_platform::auth::{OAuthClient, OAuthError, OAuthTokens, OAuthUserInfo};
use openidconnect::core::{
  CoreAuthDisplay, CoreAuthenticationFlow, CoreClaimName, CoreClaimType, CoreClient,
  CoreClientAuthMethod, CoreErrorResponseType, CoreGrantType, CoreIdToken, CoreJsonWebKey,
  CoreJweContentEncryptionAlgorithm, CoreJweKeyManagementAlgorithm, CoreResponseMode,
  CoreResponseType, CoreSubjectIdentifierType, CoreTokenResponse,
};
use openidconnect::{
  AdditionalProviderMetadata, AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret,
  CsrfToken, EndSessionUrl, EndpointMaybeSet, EndpointNotSet, EndpointSet, HttpRequest,
  HttpResponse, IssuerUrl, LogoutProviderMetadata, LogoutRequest, Nonce, OAuth2TokenResponse,
  PkceCodeChallenge, PkceCodeVerifier, PostLogoutRedirectUrl, ProviderMetadata, RedirectUrl,
  RequestTokenError, Scope, StandardErrorResponse, TokenResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use subtle::ConstantTimeEq;

pub use routes::{
  DEFAULT_CALLBACK_PATH, DEFAULT_FLOW_COOKIE, DEFAULT_LOGIN_PATH, DEFAULT_LOGOUT_PATH,
  FLOW_TIMEOUT, OidcRoutes, SESSION_ID_TOKEN_ATTR,
};

use crate::error::AuthError;
use crate::jwks::JwksClient;
use crate::jwt::{ClaimMapping, DEFAULT_LEEWAY, JwtVerifier};
use crate::principal::{AuthPrincipal, AuthScheme};
use crate::session::{Session, SessionError};

/// Default [`OidcConfig::timeout`] for provider requests.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// Connect timeout of provider requests (the platform HTTP client's, or [`OidcConfig::timeout`]
/// when that is shorter).
pub const DEFAULT_CONNECT_TIMEOUT: Duration =
  id_effect_platform::http::HttpClientConfig::DEFAULT_CONNECT_TIMEOUT;
/// Posture id of [`OidcConfig::insecure_allow_http`].
pub const INSECURE_ALLOW_HTTP: &str = "id_effect_auth::oidc::OidcConfig::insecure_allow_http";

/// Flow-session attribute holding the pending login's `state`.
const STATE_ATTR: &str = "oidc_state";
/// Flow-session attribute holding the pending login's `nonce`.
const NONCE_ATTR: &str = "oidc_nonce";
/// Flow-session attribute holding the pending login's PKCE code verifier.
const VERIFIER_ATTR: &str = "oidc_pkce_verifier";
/// Flow-session attribute holding where to go after the login.
const RETURN_TO_ATTR: &str = "oidc_return_to";

/// Why an OpenID Connect login did not complete.
///
/// [`http_status`](Self::http_status) is what the callback route answers: **400** for a
/// callback that does not belong to a pending login, **401** when the provider or the ID token
/// check refused the user, **502/503** when the provider or the session store failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OidcError {
  /// A configured URL is not a valid URL (a deployment error).
  Config(String),
  /// The provider's discovery document could not be fetched, parsed or trusted.
  Discovery(String),
  /// The token endpoint could not be reached or answered something unusable.
  Provider(String),
  /// The provider refused: an `error` in the callback (`access_denied`, …) or an error from the
  /// token endpoint (`invalid_grant`: a used, expired or foreign code, a wrong code verifier).
  Denied {
    /// The OAuth error code.
    error: String,
    /// The provider's `error_description`, if any.
    description: Option<String>,
  },
  /// The callback does not match a pending login: no flow session, a wrong `state` or `iss`, or
  /// no `code`.
  InvalidCallback(String),
  /// The ID token was rejected (signature, issuer, audience, expiry, nonce, claims).
  IdToken(AuthError),
  /// The session store failed.
  Session(SessionError),
}

impl OidcError {
  /// 400, 401, 500, 502 or 503 (see the type documentation).
  pub fn http_status(&self) -> u16 {
    match self {
      Self::Config(_) => 500,
      Self::Discovery(_) | Self::Session(_) => 503,
      Self::Provider(_) => 502,
      Self::Denied { .. } => 401,
      Self::InvalidCallback(_) => 400,
      Self::IdToken(e) => e.http_status(),
    }
  }
}

impl fmt::Display for OidcError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(m) => write!(f, "OIDC configuration: {m}"),
      Self::Discovery(m) => write!(f, "OIDC discovery: {m}"),
      Self::Provider(m) => write!(f, "OIDC provider: {m}"),
      Self::Denied {
        error,
        description: Some(d),
      } => write!(f, "the identity provider refused the login: {error} ({d})"),
      Self::Denied { error, .. } => write!(f, "the identity provider refused the login: {error}"),
      Self::InvalidCallback(m) => write!(f, "invalid login callback: {m}"),
      Self::IdToken(e) => write!(f, "ID token rejected: {e}"),
      Self::Session(e) => write!(f, "OIDC login: {e}"),
    }
  }
}

impl std::error::Error for OidcError {}

impl From<OidcError> for OAuthError {
  fn from(e: OidcError) -> Self {
    match e {
      OidcError::Denied { .. } => Self::InvalidCode,
      other => Self::Protocol(other.to_string()),
    }
  }
}

/// An error and its sources, `: `-separated (reqwest's `Display` alone hides the cause).
fn describe(e: &dyn std::error::Error) -> String {
  let mut out = e.to_string();
  let mut source = e.source();
  while let Some(cause) = source {
    out.push_str(": ");
    out.push_str(&cause.to_string());
    source = cause.source();
  }
  out
}

/// The relying party's registration at the provider, and how its ID tokens are checked.
#[derive(Clone)]
pub struct OidcConfig {
  issuer: String,
  client_id: String,
  client_secret: Option<String>,
  redirect_url: String,
  scopes: Vec<String>,
  claims: ClaimMapping,
  post_logout_redirect_url: Option<String>,
  timeout: Duration,
  leeway: Duration,
  jwks_min_refresh: Option<Duration>,
  clock: WallClock,
  allow_http: bool,
}

impl fmt::Debug for OidcConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OidcConfig")
      .field("issuer", &self.issuer)
      .field("client_id", &self.client_id)
      .field(
        "client_secret",
        &self.client_secret.as_ref().map(|_| "<redacted>"),
      )
      .field("redirect_url", &self.redirect_url)
      .field("scopes", &self.scopes)
      .finish_non_exhaustive()
  }
}

impl OidcConfig {
  /// A public client `client_id` at the provider whose issuer URL is `issuer`, receiving the
  /// code at `redirect_url` (the callback route's absolute URL, registered at the provider).
  /// Scopes `openid profile email`, the default [`ClaimMapping`].
  pub fn new(
    issuer: impl Into<String>,
    client_id: impl Into<String>,
    redirect_url: impl Into<String>,
  ) -> Self {
    Self {
      issuer: issuer.into(),
      client_id: client_id.into(),
      client_secret: None,
      redirect_url: redirect_url.into(),
      scopes: vec!["profile".into(), "email".into()],
      claims: ClaimMapping::default(),
      post_logout_redirect_url: None,
      timeout: DEFAULT_TIMEOUT,
      leeway: DEFAULT_LEEWAY,
      jwks_min_refresh: None,
      clock: WallClock::system(),
      allow_http: false,
    }
  }

  /// A confidential client: authenticate to the token endpoint with `secret`
  /// (`client_secret_basic`).
  #[must_use]
  pub fn client_secret(mut self, secret: impl Into<String>) -> Self {
    self.client_secret = Some(secret.into());
    self
  }

  /// Request these scopes (besides `openid`, which is always requested).
  #[must_use]
  pub fn scopes<S: Into<String>>(mut self, scopes: impl IntoIterator<Item = S>) -> Self {
    self.scopes = scopes
      .into_iter()
      .map(Into::into)
      .filter(|s| s != "openid")
      .collect();
    self
  }

  /// Map ID-token claims to the principal with `mapping` (for Keycloak realm roles:
  /// `ClaimMapping::default().roles(["realm_access.roles"])`).
  #[must_use]
  pub fn claims(mut self, mapping: ClaimMapping) -> Self {
    self.claims = mapping;
    self
  }

  /// Where the provider sends the browser after an RP-initiated logout (registered at the
  /// provider).
  #[must_use]
  pub fn post_logout_redirect_url(mut self, url: impl Into<String>) -> Self {
    self.post_logout_redirect_url = Some(url.into());
    self
  }

  /// Give up on a provider request after `timeout` (default [`DEFAULT_TIMEOUT`]).
  #[must_use]
  pub fn timeout(mut self, timeout: Duration) -> Self {
    self.timeout = timeout;
    self
  }

  /// Clock-skew allowance for the ID token's `exp` and `nbf` (default [`DEFAULT_LEEWAY`]).
  #[must_use]
  pub fn leeway(mut self, leeway: Duration) -> Self {
    self.leeway = leeway;
    self
  }

  /// [`JwksClient::min_refresh_interval`] of the provider's key set.
  #[must_use]
  pub fn jwks_min_refresh_interval(mut self, interval: Duration) -> Self {
    self.jwks_min_refresh = Some(interval);
    self
  }

  /// Talk to the provider (discovery, token, user-info and JWKS endpoints) over plain `http`
  /// from any host, not only loopback ones. Records [`INSECURE_ALLOW_HTTP`] in the posture
  /// registry.
  ///
  /// **Insecure**: anyone on the network path can then read the client secret and the tokens,
  /// or swap the key set and have ID tokens they signed accepted. Only for a provider reached
  /// over a network that is itself trusted and encrypted (a service mesh with mTLS).
  #[must_use]
  pub fn insecure_allow_http(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      INSECURE_ALLOW_HTTP,
      "M3: OIDC discovery, token and JWKS requests may go over plain http to any host",
    );
    self.allow_http = true;
    self
  }

  /// Check ID-token times against `clock` (an anchored clock follows a `TestExecutor`).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// The configured issuer URL.
  #[inline]
  pub fn issuer(&self) -> &str {
    &self.issuer
  }

  /// The client id.
  #[inline]
  pub fn client_id(&self) -> &str {
    &self.client_id
  }
}

/// An authorization redirect and the login state that must stay on the server until the
/// callback ([`OidcClient::begin`]).
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
  /// Send the browser here.
  pub url: String,
  /// Keep this server-side (in a session), keyed to the browser that follows `url`.
  pub pending: PendingLogin,
}

impl fmt::Debug for AuthorizationRequest {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AuthorizationRequest")
      .field("url", &self.url)
      .finish_non_exhaustive()
  }
}

/// The secrets of a login in progress: `state` (CSRF), `nonce` (ID-token replay) and the PKCE
/// code verifier, plus where to return afterwards. `Debug` hides the secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingLogin {
  state: String,
  nonce: String,
  pkce_verifier: String,
  return_to: Option<String>,
}

impl fmt::Debug for PendingLogin {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PendingLogin")
      .field("return_to", &self.return_to)
      .finish_non_exhaustive()
  }
}

impl PendingLogin {
  /// The `state` sent to the provider.
  #[inline]
  pub fn state(&self) -> &str {
    &self.state
  }

  /// The `nonce` the ID token must carry.
  #[inline]
  pub fn nonce(&self) -> &str {
    &self.nonce
  }

  /// Where to send the browser after the login, if the login request said.
  #[inline]
  pub fn return_to(&self) -> Option<&str> {
    self.return_to.as_deref()
  }

  /// The same login, returning to `path` afterwards.
  #[must_use]
  pub fn with_return_to(mut self, path: impl Into<String>) -> Self {
    self.return_to = Some(path.into());
    self
  }

  /// `true` when the callback's `state` is this login's (constant-time comparison).
  pub fn state_matches(&self, state: &str) -> bool {
    bool::from(self.state.as_bytes().ct_eq(state.as_bytes()))
  }

  /// Record the login in `session`'s attributes (save the session afterwards).
  pub fn store(&self, session: &mut Session) {
    session
      .set_attr(STATE_ATTR, self.state.as_str())
      .set_attr(NONCE_ATTR, self.nonce.as_str())
      .set_attr(VERIFIER_ATTR, self.pkce_verifier.as_str());
    if let Some(path) = &self.return_to {
      session.set_attr(RETURN_TO_ATTR, path.as_str());
    }
  }

  /// The login [`store`](Self::store)d in `session`, if there is one.
  pub fn from_session(session: &Session) -> Option<Self> {
    Some(Self {
      state: session.attr(STATE_ATTR)?.to_owned(),
      nonce: session.attr(NONCE_ATTR)?.to_owned(),
      pkce_verifier: session.attr(VERIFIER_ATTR)?.to_owned(),
      return_to: session.attr(RETURN_TO_ATTR).map(str::to_owned),
    })
  }
}

/// A completed login: the principal of the verified ID token, the raw ID token (the
/// `id_token_hint` of a later logout) and the provider's tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct OidcLogin {
  /// The ID token's claims through the [`ClaimMapping`], scheme [`AuthScheme::Session`].
  pub principal: AuthPrincipal,
  /// The ID token as received.
  pub id_token: String,
  /// Access token, refresh token and lifetime from the token endpoint.
  pub tokens: OAuthTokens,
}

impl fmt::Debug for OidcLogin {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OidcLogin")
      .field("principal", &self.principal)
      .finish_non_exhaustive()
  }
}

/// openidconnect's client after discovery: authorization endpoint set, token and user-info
/// endpoints as the provider published them.
type ProviderClient = CoreClient<
  EndpointSet,
  EndpointNotSet,
  EndpointNotSet,
  EndpointNotSet,
  EndpointMaybeSet,
  EndpointMaybeSet,
>;

/// The discovery metadata read beyond OpenID Connect Discovery's: RFC 8414's PKCE methods
/// (inside openidconnect's RP-initiated logout metadata).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct PkceMetadata {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  code_challenge_methods_supported: Option<Vec<String>>,
}

impl AdditionalProviderMetadata for PkceMetadata {}

/// The provider metadata [`OidcClient::discover`] reads.
type Metadata = ProviderMetadata<
  LogoutProviderMetadata<PkceMetadata>,
  CoreAuthDisplay,
  CoreClientAuthMethod,
  CoreClaimName,
  CoreClaimType,
  CoreGrantType,
  CoreJweContentEncryptionAlgorithm,
  CoreJweKeyManagementAlgorithm,
  CoreJsonWebKey,
  CoreResponseMode,
  CoreResponseType,
  CoreSubjectIdentifierType,
>;

type HttpFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, reqwest::Error>> + Send>>;

/// openidconnect's HTTP client: reqwest 0.13 with rustls, no redirects.
#[derive(Clone)]
struct Http(reqwest::Client);

impl<'c> AsyncHttpClient<'c> for Http {
  type Error = reqwest::Error;
  type Future = HttpFuture;

  fn call(&'c self, request: HttpRequest) -> Self::Future {
    let http = self.0.clone();
    Box::pin(async move {
      let response = http.execute(reqwest::Request::try_from(request)?).await?;
      let status = response.status();
      let headers = response.headers().clone();
      let mut out = HttpResponse::new(response.bytes().await?.to_vec());
      *out.status_mut() = status;
      *out.headers_mut() = headers;
      Ok(out)
    })
  }
}

struct Inner {
  config: OidcConfig,
  provider: ProviderClient,
  issuer: String,
  authorization_endpoint: String,
  end_session_endpoint: Option<EndSessionUrl>,
  post_logout_redirect_url: Option<PostLogoutRedirectUrl>,
  jwks: JwksClient,
  verifier: JwtVerifier,
  http: Http,
}

/// The relying party of one OpenID Connect provider, after discovery. Cheap to clone. See the
/// [module documentation](self).
#[derive(Clone)]
pub struct OidcClient {
  inner: Arc<Inner>,
}

impl fmt::Debug for OidcClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("OidcClient")
      .field("issuer", &self.inner.issuer)
      .field("client_id", &self.inner.config.client_id)
      .field("jwks", &self.inner.jwks.url())
      .finish_non_exhaustive()
  }
}

impl OidcClient {
  /// Fetch the provider's discovery document and build the client.
  ///
  /// Fails with [`OidcError::Config`] for an invalid configured URL and
  /// [`OidcError::Discovery`] when the document cannot be fetched or parsed, names another
  /// issuer, or says the provider does not support PKCE `S256`.
  pub fn discover(config: OidcConfig) -> Effect<Self, OidcError> {
    from_async(move || Self::discover_now(config))
  }

  async fn discover_now(config: OidcConfig) -> Result<Self, OidcError> {
    let url = |what: &str, e: openidconnect::url::ParseError| {
      OidcError::Config(format!("{what} is not a valid URL: {e}"))
    };
    let issuer = IssuerUrl::new(config.issuer.clone()).map_err(|e| url("issuer", e))?;
    require_https("issuer", issuer.as_str(), config.allow_http).map_err(OidcError::Config)?;
    let redirect =
      RedirectUrl::new(config.redirect_url.clone()).map_err(|e| url("redirect_url", e))?;
    let post_logout_redirect_url = config
      .post_logout_redirect_url
      .clone()
      .map(PostLogoutRedirectUrl::new)
      .transpose()
      .map_err(|e| url("post_logout_redirect_url", e))?;
    let http = reqwest::Client::builder()
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(config.timeout.min(DEFAULT_CONNECT_TIMEOUT))
      .timeout(config.timeout)
      .build()
      .map(Http)
      .map_err(|e| OidcError::Discovery(describe(&e)))?;
    let metadata = Metadata::discover_async(issuer, &http)
      .await
      .map_err(|e| OidcError::Discovery(describe(&e)))?;
    let pkce = &metadata
      .additional_metadata()
      .additional_metadata
      .code_challenge_methods_supported;
    if let Some(methods) = pkce
      && !methods.iter().any(|m| m == "S256")
    {
      return Err(OidcError::Discovery(
        "the provider does not support PKCE S256".into(),
      ));
    }
    let endpoints = [
      (
        "token_endpoint",
        metadata.token_endpoint().map(|u| u.as_str()),
      ),
      (
        "userinfo_endpoint",
        metadata.userinfo_endpoint().map(|u| u.as_str()),
      ),
      ("jwks_uri", Some(metadata.jwks_uri().as_str())),
    ];
    for (what, endpoint) in endpoints {
      if let Some(endpoint) = endpoint {
        require_https(what, endpoint, config.allow_http).map_err(OidcError::Discovery)?;
      }
    }
    let issuer = metadata.issuer().as_str().to_owned();
    let authorization_endpoint = metadata.authorization_endpoint().as_str().to_owned();
    let end_session_endpoint = metadata.additional_metadata().end_session_endpoint.clone();
    let mut jwks = JwksClient::new(metadata.jwks_uri().as_str()).timeout(config.timeout);
    if let Some(interval) = config.jwks_min_refresh {
      jwks = jwks.min_refresh_interval(interval);
    }
    if config.allow_http {
      jwks = jwks.allow_http_dangerous();
    }
    let verifier = JwtVerifier::jwks(jwks.clone())
      .issuer(issuer.clone())
      .audience(config.client_id.clone())
      .require_claim("sub")
      .require_claim("iat")
      .leeway(config.leeway)
      .with_clock(config.clock.clone());
    let provider = CoreClient::from_provider_metadata(
      metadata,
      ClientId::new(config.client_id.clone()),
      config.client_secret.clone().map(ClientSecret::new),
    )
    .set_redirect_uri(redirect);
    Ok(Self {
      inner: Arc::new(Inner {
        config,
        provider,
        issuer,
        authorization_endpoint,
        end_session_endpoint,
        post_logout_redirect_url,
        jwks,
        verifier,
        http,
      }),
    })
  }

  /// The provider's issuer (as discovered; equal to the configured one).
  #[inline]
  pub fn issuer(&self) -> &str {
    &self.inner.issuer
  }

  /// The client id.
  #[inline]
  pub fn client_id(&self) -> &str {
    &self.inner.config.client_id
  }

  /// The provider's authorization endpoint.
  #[inline]
  pub fn authorization_endpoint(&self) -> &str {
    &self.inner.authorization_endpoint
  }

  /// The provider's RP-initiated logout endpoint, if it publishes one.
  pub fn end_session_endpoint(&self) -> Option<&str> {
    self
      .inner
      .end_session_endpoint
      .as_ref()
      .map(|url| url.url().as_str())
  }

  /// The provider's key set (shared with the ID-token verifier).
  #[inline]
  pub fn jwks(&self) -> &JwksClient {
    &self.inner.jwks
  }

  /// Start a login: the authorization URL (response type `code`, the configured scopes, a fresh
  /// random `state` and `nonce`, PKCE `S256`) and the [`PendingLogin`] to keep server-side.
  pub fn begin(&self) -> AuthorizationRequest {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state, nonce) = self
      .inner
      .config
      .scopes
      .iter()
      .fold(
        self.inner.provider.authorize_url(
          CoreAuthenticationFlow::AuthorizationCode,
          CsrfToken::new_random,
          Nonce::new_random,
        ),
        |request, scope| request.add_scope(Scope::new(scope.clone())),
      )
      .set_pkce_challenge(challenge)
      .url();
    AuthorizationRequest {
      url: url.into(),
      pending: PendingLogin {
        state: state.into_secret(),
        nonce: nonce.secret().clone(),
        pkce_verifier: verifier.into_secret(),
        return_to: None,
      },
    }
  }

  /// Finish the login `pending` with the callback's `code`: exchange it (with the code
  /// verifier) and verify the ID token, whose `nonce` must be the pending one. Check the
  /// callback's `state` with [`PendingLogin::state_matches`] first.
  pub fn exchange(&self, code: &str, pending: &PendingLogin) -> Effect<OidcLogin, OidcError> {
    let this = self.clone();
    let nonce = pending.nonce.clone();
    self
      .request_tokens(code, Some(pending.pkce_verifier.clone()))
      .flat_map(move |(id_token, tokens)| {
        this
          .verify_id_token(&id_token, Some(&nonce))
          .map(move |principal| OidcLogin {
            principal,
            id_token,
            tokens,
          })
      })
  }

  fn request_tokens(
    &self,
    code: &str,
    pkce_verifier: Option<String>,
  ) -> Effect<(String, OAuthTokens), OidcError> {
    let (this, code) = (self.clone(), code.to_owned());
    from_async(move || async move {
      let request = this
        .inner
        .provider
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| OidcError::Discovery(describe(&e)))?;
      let request = match pkce_verifier {
        Some(v) => request.set_pkce_verifier(PkceCodeVerifier::new(v)),
        None => request,
      };
      let response: CoreTokenResponse = request
        .request_async(&this.inner.http)
        .await
        .map_err(token_error)?;
      let id_token = response
        .id_token()
        .ok_or_else(|| OidcError::Provider("the token response has no id_token".into()))?
        .to_string();
      let tokens = OAuthTokens {
        access_token: response.access_token().secret().clone(),
        refresh_token: response.refresh_token().map(|t| t.secret().clone()),
        expires_in: response.expires_in().map(|d| d.as_secs()),
      };
      Ok((id_token, tokens))
    })
  }

  /// The principal of `id_token`: signature over the provider's JWKS, `iss`, `aud` (the client
  /// id), `exp`/`nbf`, `sub` and `iat` present, `azp` (required to be the client id when there
  /// are several audiences), and `nonce` equal to `nonce` when given. Claims are mapped with the
  /// configured [`ClaimMapping`]; the scheme is [`AuthScheme::Session`].
  pub fn verify_id_token(
    &self,
    id_token: &str,
    nonce: Option<&str>,
  ) -> Effect<AuthPrincipal, OidcError> {
    let nonce = nonce.map(str::to_owned);
    let client_id = self.inner.config.client_id.clone();
    let mapping = self.inner.config.claims.clone();
    self
      .inner
      .verifier
      .verify_claims(id_token)
      .flat_map(move |claims| {
        match check_oidc_claims(&claims, nonce.as_deref(), &client_id)
          .and_then(|()| mapping.principal(claims))
        {
          Ok(p) => succeed(p.with_scheme(AuthScheme::Session)),
          Err(e) => fail(e),
        }
      })
      .map_err(OidcError::IdToken)
  }

  /// The provider's logout URL (RP-initiated logout) with `id_token_hint`, the client id and
  /// the configured post-logout redirect; `None` when the provider has no
  /// `end_session_endpoint`.
  pub fn logout_url(&self, id_token_hint: Option<&str>) -> Option<String> {
    let endpoint = self.inner.end_session_endpoint.clone()?;
    let mut request = LogoutRequest::from(endpoint)
      .set_client_id(ClientId::new(self.inner.config.client_id.clone()));
    if let Some(hint) = id_token_hint.and_then(|t| CoreIdToken::from_str(t).ok()) {
      request = request.set_id_token_hint(&hint);
    }
    if let Some(url) = &self.inner.post_logout_redirect_url {
      request = request.set_post_logout_redirect_uri(url.clone());
    }
    Some(request.http_get_url().into())
  }
}

/// `Ok` when the `what` URL `url` is `https`, or `http` to a loopback host (or any host with
/// `allow_http`); the reason otherwise.
fn require_https(what: &str, url: &str, allow_http: bool) -> Result<(), String> {
  let parsed =
    reqwest::Url::parse(url).map_err(|e| format!("{what} {url:?} is not a valid URL: {e}"))?;
  match parsed.scheme() {
    "https" => Ok(()),
    "http" if allow_http || crate::jwks::is_loopback(&parsed) => Ok(()),
    _ => Err(format!(
      "{what} {url:?} must be https (plain http only to a loopback host, or after \
       insecure_allow_http())"
    )),
  }
}

/// A token-endpoint failure as an [`OidcError`].
fn token_error(
  e: RequestTokenError<reqwest::Error, StandardErrorResponse<CoreErrorResponseType>>,
) -> OidcError {
  match e {
    RequestTokenError::ServerResponse(response) => OidcError::Denied {
      error: response.error().as_ref().to_owned(),
      description: response.error_description().cloned(),
    },
    other => OidcError::Provider(describe(&other)),
  }
}

/// The OpenID Connect checks on signature-verified ID-token claims that a plain JWT check does
/// not make: `nonce` and `azp`.
fn check_oidc_claims(
  claims: &Map<String, Value>,
  nonce: Option<&str>,
  client_id: &str,
) -> Result<(), AuthError> {
  if let Some(expected) = nonce {
    let matches = claims
      .get("nonce")
      .and_then(Value::as_str)
      .is_some_and(|n| bool::from(n.as_bytes().ct_eq(expected.as_bytes())));
    if !matches {
      return Err(AuthError::InvalidClaims(
        "nonce does not match the login".into(),
      ));
    }
  }
  let several_audiences = claims
    .get("aud")
    .and_then(Value::as_array)
    .is_some_and(|a| a.len() > 1);
  match claims.get("azp").map(Value::as_str) {
    Some(Some(azp)) if azp == client_id => Ok(()),
    None if !several_audiences => Ok(()),
    None => Err(AuthError::InvalidClaims(
      "azp is required with several audiences".into(),
    )),
    Some(_) => Err(AuthError::InvalidClaims("azp is not this client".into())),
  }
}

/// The plain OAuth 2.0 view, for code written against id_effect_platform's capability. The
/// trait carries no code verifier or nonce: [`authorization_url`](OAuthClient::authorization_url)
/// sends neither PKCE nor a checked nonce, so a provider that enforces PKCE refuses it. Browser
/// logins should use [`OidcClient::begin`] / [`OidcClient::exchange`] (or [`OidcRoutes`]).
impl OAuthClient for OidcClient {
  fn authorization_url(&self, state: &str) -> String {
    let state = state.to_owned();
    let (url, _, _) = self
      .inner
      .config
      .scopes
      .iter()
      .fold(
        self.inner.provider.authorize_url(
          CoreAuthenticationFlow::AuthorizationCode,
          move || CsrfToken::new(state),
          Nonce::new_random,
        ),
        |request, scope| request.add_scope(Scope::new(scope.clone())),
      )
      .url();
    url.into()
  }

  fn exchange_code(&self, code: &str) -> Effect<(OAuthTokens, OAuthUserInfo), OAuthError> {
    let this = self.clone();
    self
      .request_tokens(code, None)
      .flat_map(move |(id_token, tokens)| {
        this.verify_id_token(&id_token, None).map(move |p| {
          use id_effect_authz::Principal as _;
          let claim = |name: &str| p.claim(name).and_then(Value::as_str).map(str::to_owned);
          let user = OAuthUserInfo {
            sub: claim("sub").unwrap_or_else(|| p.id().to_owned()),
            email: claim("email"),
          };
          (tokens, user)
        })
      })
      .map_err(OAuthError::from)
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  fn claims(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
  }

  #[test]
  fn nonce_and_azp_checks() {
    let ok = claims(json!({"nonce": "n1", "aud": "web"}));
    assert_eq!(check_oidc_claims(&ok, Some("n1"), "web"), Ok(()));
    assert_eq!(check_oidc_claims(&ok, None, "web"), Ok(()));
    let wrong = check_oidc_claims(&ok, Some("n2"), "web").unwrap_err();
    assert!(wrong.to_string().contains("nonce"), "{wrong}");
    let missing = claims(json!({"aud": "web"}));
    assert!(check_oidc_claims(&missing, Some("n1"), "web").is_err());

    let several = claims(json!({"aud": ["web", "api"]}));
    let e = check_oidc_claims(&several, None, "web").unwrap_err();
    assert!(e.to_string().contains("azp is required"), "{e}");
    let with_azp = claims(json!({"aud": ["web", "api"], "azp": "web"}));
    assert_eq!(check_oidc_claims(&with_azp, None, "web"), Ok(()));
    let foreign = claims(json!({"aud": "web", "azp": "other"}));
    let e = check_oidc_claims(&foreign, None, "web").unwrap_err();
    assert!(e.to_string().contains("not this client"), "{e}");
    let not_string = claims(json!({"aud": "web", "azp": 7}));
    assert!(check_oidc_claims(&not_string, None, "web").is_err());
  }

  #[test]
  fn error_statuses_messages_and_oauth_mapping() {
    let all = [
      (OidcError::Config("x".into()), 500, "configuration"),
      (OidcError::Discovery("x".into()), 503, "discovery"),
      (OidcError::Provider("x".into()), 502, "provider: x"),
      (
        OidcError::Denied {
          error: "access_denied".into(),
          description: Some("no".into()),
        },
        401,
        "access_denied (no)",
      ),
      (
        OidcError::Denied {
          error: "invalid_grant".into(),
          description: None,
        },
        401,
        "refused the login: invalid_grant",
      ),
      (OidcError::InvalidCallback("x".into()), 400, "callback"),
      (OidcError::IdToken(AuthError::Expired), 401, "expired"),
      (
        OidcError::IdToken(AuthError::Unavailable("jwks".into())),
        503,
        "unavailable",
      ),
      (
        OidcError::Session(SessionError::Storage("down".into())),
        503,
        "storage: down",
      ),
    ];
    for (e, status, text) in all {
      assert_eq!(e.http_status(), status, "{e:?}");
      assert!(e.to_string().contains(text), "{e} lacks {text}");
    }
    let denied = OidcError::Denied {
      error: "invalid_grant".into(),
      description: None,
    };
    assert_eq!(OAuthError::from(denied), OAuthError::InvalidCode);
    assert!(matches!(
      OAuthError::from(OidcError::Provider("down".into())),
      OAuthError::Protocol(m) if m.contains("down")
    ));
  }

  #[test]
  fn describe_joins_sources() {
    #[derive(Debug)]
    struct Outer(std::io::Error);
    impl fmt::Display for Outer {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("outer")
      }
    }
    impl std::error::Error for Outer {
      fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
      }
    }
    let e = Outer(std::io::Error::other("inner"));
    assert_eq!(describe(&e), "outer: inner");
  }

  #[test]
  fn config_builder_and_debug_redact_secrets() {
    let c = OidcConfig::new("https://id.example.com", "web", "https://app/cb")
      .client_secret("hunter2")
      .scopes(["openid", "email"])
      .post_logout_redirect_url("https://app/")
      .timeout(Duration::from_secs(3))
      .leeway(Duration::from_secs(5))
      .jwks_min_refresh_interval(Duration::from_secs(7));
    assert_eq!(
      (c.issuer(), c.client_id()),
      ("https://id.example.com", "web")
    );
    assert_eq!(c.scopes, ["email"], "openid is implicit");
    assert_eq!(
      (c.timeout, c.leeway, c.jwks_min_refresh),
      (
        Duration::from_secs(3),
        Duration::from_secs(5),
        Some(Duration::from_secs(7))
      )
    );
    let shown = format!("{c:?}");
    assert!(
      shown.contains("<redacted>") && !shown.contains("hunter2"),
      "{shown}"
    );
  }

  #[test]
  fn pending_login_round_trips_through_a_session() {
    use std::collections::BTreeMap;

    use crate::session::SessionId;

    let pending = PendingLogin {
      state: "s".into(),
      nonce: "n".into(),
      pkce_verifier: "v".into(),
      return_to: None,
    }
    .with_return_to("/ledger");
    let t = id_effect::UtcDateTime::unsafe_make(0);
    let mut session = Session::from_parts(
      SessionId::generate().unwrap(),
      "flow",
      BTreeMap::new(),
      t,
      t,
      t,
      t,
    );
    assert_eq!(PendingLogin::from_session(&session), None);
    pending.store(&mut session);
    let back = PendingLogin::from_session(&session).unwrap();
    assert_eq!(back, pending);
    assert_eq!(
      (back.state(), back.nonce(), back.return_to()),
      ("s", "n", Some("/ledger"))
    );
    assert!(back.state_matches("s") && !back.state_matches("t") && !back.state_matches(""));
    let shown = format!("{back:?}");
    assert!(
      !shown.contains("\"v\"") && shown.contains("/ledger"),
      "{shown}"
    );
    let request = AuthorizationRequest {
      url: "https://id/auth".into(),
      pending: back,
    };
    assert!(!format!("{request:?}").contains("nonce"));
  }
}
