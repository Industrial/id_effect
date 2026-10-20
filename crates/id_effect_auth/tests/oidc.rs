//! OpenID Connect login against a mocked identity provider (wiremock serves discovery, JWKS and
//! the token endpoint; ID tokens are signed with a test key): discovery, authorization code +
//! PKCE, state and nonce in the flow session, the ID-token checks, the login, callback and
//! logout routes (sec.authn #5). `tests/oidc_keycloak.rs` runs the same flow against Keycloak.
#![cfg(feature = "oidc")]

mod common;

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::header::{AUTHORIZATION, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use id_effect::{Cause, Effect, Exit};
use id_effect_auth::oidc::{OidcClient, OidcConfig, OidcError, OidcRoutes, SESSION_ID_TOKEN_ATTR};
use id_effect_auth::{
  AuthError, AuthPrincipal, AuthScheme, Authenticate, ClaimMapping, JwtIssuer, MemorySessionStore,
  SessionConfig, Sessions,
};
use id_effect_authz::Principal;
use id_effect_platform::auth::{OAuthClient, OAuthError};
use id_effect_tokio::TokioRuntime;
use openidconnect::url::Url;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "web";
const SECRET: &str = "web-secret";
const REDIRECT: &str = "https://app.example/auth/oidc/callback";

/// Runs `eff` on the ambient tokio runtime (reqwest needs its IO driver).
async fn run<A, E>(eff: Effect<A, E>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + fmt::Debug + 'static,
{
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// A mocked provider: discovery, JWKS with the signer's public key, and (per test) the token
/// endpoint.
struct Idp {
  server: MockServer,
  issuer: String,
  signer: JwtIssuer,
}

impl Idp {
  async fn start() -> Self {
    Self::with_discovery(|_| {}).await
  }

  /// A provider whose discovery document `edit` changed.
  async fn with_discovery(edit: impl FnOnce(&mut Map<String, Value>)) -> Self {
    let server = MockServer::start().await;
    let issuer = format!("{}/realms/test", server.uri());
    let signer = common::ec_issuer("idp-1")
      .issuer(issuer.clone())
      .audience(CLIENT_ID);
    let mut doc = json!({
      "issuer": issuer,
      "authorization_endpoint": format!("{issuer}/auth"),
      "token_endpoint": format!("{issuer}/token"),
      "jwks_uri": format!("{issuer}/certs"),
      "end_session_endpoint": format!("{issuer}/logout"),
      "response_types_supported": ["code"],
      "subject_types_supported": ["public"],
      "id_token_signing_alg_values_supported": ["ES256", "RS256"],
      "code_challenge_methods_supported": ["plain", "S256"]
    });
    edit(doc.as_object_mut().unwrap());
    Mock::given(method("GET"))
      .and(path("/realms/test/.well-known/openid-configuration"))
      .respond_with(ResponseTemplate::new(200).set_body_json(doc))
      .mount(&server)
      .await;
    let keys = json!({ "keys": [signer.jwk().unwrap()] });
    Mock::given(method("GET"))
      .and(path("/realms/test/certs"))
      .respond_with(ResponseTemplate::new(200).set_body_json(keys))
      .mount(&server)
      .await;
    Self {
      server,
      issuer,
      signer,
    }
  }

  fn config(&self) -> OidcConfig {
    OidcConfig::new(self.issuer.clone(), CLIENT_ID, REDIRECT)
      .client_secret(SECRET)
      .post_logout_redirect_url("https://app.example/")
      .timeout(Duration::from_secs(5))
      .jwks_min_refresh_interval(Duration::ZERO)
  }

  async fn client(&self) -> OidcClient {
    run(OidcClient::discover(self.config())).await.unwrap()
  }

  /// An ID token for alice (an accountant) with `extra` claims, signed by `signer`.
  async fn id_token_by(&self, signer: &JwtIssuer, extra: Value) -> String {
    let mut claims = json!({
      "sub": "alice",
      "roles": ["accountant"],
      "email": "alice@example.com",
      "tenant": "acme"
    })
    .as_object()
    .cloned()
    .unwrap();
    claims.extend(extra.as_object().cloned().unwrap());
    run(signer.issue_claims(claims)).await.unwrap()
  }

  async fn id_token(&self, extra: Value) -> String {
    self.id_token_by(&self.signer, extra).await
  }

  /// The next token request answers `status` with `body`.
  async fn answer_token(&self, status: u16, body: Value) {
    Mock::given(method("POST"))
      .and(path("/realms/test/token"))
      .respond_with(ResponseTemplate::new(status).set_body_json(body))
      .up_to_n_times(1)
      .mount(&self.server)
      .await;
  }

  async fn answer_tokens(&self, id_token: &str) {
    self.answer_token(200, tokens(Some(id_token))).await;
  }

  /// The form fields of the last token request, and its `Authorization` header.
  async fn last_token_request(&self) -> (HashMap<String, String>, String) {
    let requests = self.server.received_requests().await.unwrap();
    let request = requests
      .iter()
      .rev()
      .find(|r| r.url.path().ends_with("/token"))
      .expect("a token request");
    let form = openidconnect::url::form_urlencoded::parse(&request.body)
      .into_owned()
      .collect();
    let auth = request
      .headers
      .get(AUTHORIZATION)
      .map(|v| v.to_str().unwrap().to_owned())
      .unwrap_or_default();
    (form, auth)
  }
}

fn tokens(id_token: Option<&str>) -> Value {
  let mut body = json!({
    "access_token": "access-1",
    "token_type": "Bearer",
    "expires_in": 300,
    "refresh_token": "refresh-1"
  });
  if let Some(t) = id_token {
    body["id_token"] = json!(t);
  }
  body
}

fn params(url: &str) -> HashMap<String, String> {
  Url::parse(url)
    .unwrap()
    .query_pairs()
    .into_owned()
    .collect()
}

/// An application: `/me` answers the request's principal; the OIDC routes; `Authenticate` with
/// the application's sessions.
fn app(routes: OidcRoutes, sessions: &Sessions) -> Router {
  Router::new()
    .route(
      "/me",
      get(|p: Option<Extension<AuthPrincipal>>| async move {
        Json(match p {
          Some(Extension(p)) => json!({"id": p.id(), "roles": p.roles(), "tenant": p.tenant()}),
          None => json!({"id": null}),
        })
      }),
    )
    .merge(routes.router())
    .layer(Authenticate::session(sessions.clone()))
}

async fn send(app: &Router, method: &str, uri: &str, cookies: &[&str]) -> Response {
  let mut request = Request::builder().method(method).uri(uri);
  if !cookies.is_empty() {
    request = request.header(COOKIE, cookies.join("; "));
  }
  app
    .clone()
    .oneshot(request.body(Body::empty()).unwrap())
    .await
    .unwrap()
}

/// The `name=value` of the cookie `name` the response sets, and its attributes.
fn set_cookie(response: &Response, name: &str) -> Option<(String, String)> {
  response
    .headers()
    .get_all(SET_COOKIE)
    .iter()
    .map(|v| v.to_str().unwrap())
    .find(|v| v.starts_with(&format!("{name}=")))
    .map(|v| {
      let (pair, attrs) = v.split_once(';').unwrap_or((v, ""));
      (pair.to_owned(), attrs.to_owned())
    })
}

async fn json_body(response: Response) -> Value {
  let bytes = response.into_body().collect().await.unwrap().to_bytes();
  serde_json::from_slice(&bytes).unwrap()
}

fn location(response: &Response) -> String {
  response.headers()[LOCATION].to_str().unwrap().to_owned()
}

const FLOW: &str = "__Host-id_oidc_flow";
const SESSION: &str = "__Host-id_session";

#[tokio::test]
async fn login_with_pkce_state_and_nonce_creates_a_session() {
  let idp = Idp::start().await;
  let client = idp.client().await;
  assert_eq!(client.issuer(), idp.issuer);
  assert_eq!(client.client_id(), CLIENT_ID);
  assert_eq!(
    client.authorization_endpoint(),
    format!("{}/auth", idp.issuer)
  );
  assert!(client.jwks().url().ends_with("/certs"));
  assert!(format!("{client:?}").contains(CLIENT_ID));
  let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
  let routes = OidcRoutes::new(client, sessions.clone());
  assert!(format!("{routes:?}").contains("/auth/oidc/callback"));
  let app = app(routes, &sessions);

  // 1. Login: a flow session and a redirect to the provider with PKCE S256, state and nonce.
  let started = send(&app, "GET", "/auth/oidc/login?return_to=/ledger", &[]).await;
  assert_eq!(started.status(), StatusCode::SEE_OTHER);
  let auth_url = location(&started);
  assert!(
    auth_url.starts_with(&format!("{}/auth?", idp.issuer)),
    "{auth_url}"
  );
  let q = params(&auth_url);
  assert_eq!(q["response_type"], "code");
  assert_eq!(q["client_id"], CLIENT_ID);
  assert_eq!(q["redirect_uri"], REDIRECT);
  assert_eq!(q["scope"], "openid profile email");
  assert_eq!(q["code_challenge_method"], "S256");
  let (state, nonce) = (q["state"].clone(), q["nonce"].clone());
  assert!(state.len() >= 22 && nonce.len() >= 22);
  let (flow, attrs) = set_cookie(&started, FLOW).expect("flow cookie");
  assert!(
    attrs.contains("HttpOnly") && attrs.contains("SameSite=Lax"),
    "{attrs}"
  );
  assert!(
    set_cookie(&started, SESSION).is_none(),
    "no application session yet"
  );
  assert_eq!(
    json_body(send(&app, "GET", "/me", &[&flow]).await).await["id"],
    Value::Null
  );

  // 2. The provider redirects back; the code is exchanged with the verifier.
  idp
    .answer_tokens(
      &idp
        .id_token(json!({"nonce": nonce, "azp": CLIENT_ID}))
        .await,
    )
    .await;
  let callback = format!(
    "/auth/oidc/callback?code=code-1&state={state}&iss={}",
    idp.issuer
  );
  let done = send(&app, "GET", &callback, &[&flow]).await;
  assert_eq!(
    done.status(),
    StatusCode::SEE_OTHER,
    "{:?}",
    json_body(done).await
  );
  assert_eq!(location(&done), "/ledger");
  let (session, attrs) = set_cookie(&done, SESSION).expect("session cookie");
  assert!(attrs.contains("HttpOnly"), "{attrs}");
  let (cleared, attrs) = set_cookie(&done, FLOW).expect("flow cookie cleared");
  assert!(attrs.contains("Max-Age=0"), "{cleared}{attrs}");

  let (form, auth) = idp.last_token_request().await;
  assert_eq!(form["grant_type"], "authorization_code");
  assert_eq!(form["code"], "code-1");
  assert_eq!(form["redirect_uri"], REDIRECT);
  let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
  assert_eq!(
    challenge, q["code_challenge"],
    "the PKCE verifier matches the challenge"
  );
  assert!(auth.starts_with("Basic "), "client_secret_basic: {auth}");

  // 3. The session authenticates the application's routes.
  let me = json_body(send(&app, "GET", "/me", &[&session]).await).await;
  assert_eq!(
    me,
    json!({"id": "alice", "roles": ["accountant"], "tenant": "acme"})
  );

  let id = sessions.session_id(&session).expect("session id");
  let stored = run(sessions.load(&id))
    .await
    .unwrap()
    .expect("live session");
  assert_eq!(stored.user_id(), "alice");
  assert_eq!(stored.attr("roles"), Some("accountant"));
  assert!(
    stored
      .attr(SESSION_ID_TOKEN_ATTR)
      .is_some_and(|t| t.starts_with("ey"))
  );

  // 4. The callback URL works once.
  let replay = send(&app, "GET", &callback, &[&flow]).await;
  assert_eq!(replay.status(), StatusCode::BAD_REQUEST);

  // 5. Logout ends the session and continues to the provider (RP-initiated logout).
  let out = send(&app, "POST", "/auth/oidc/logout", &[&session]).await;
  assert_eq!(out.status(), StatusCode::SEE_OTHER);
  let logout = location(&out);
  assert!(
    logout.starts_with(&format!("{}/logout?", idp.issuer)),
    "{logout}"
  );
  let lq = params(&logout);
  assert_eq!(lq["client_id"], CLIENT_ID);
  assert_eq!(lq["post_logout_redirect_uri"], "https://app.example/");
  assert!(lq["id_token_hint"].starts_with("ey"));
  assert!(set_cookie(&out, SESSION).unwrap().1.contains("Max-Age=0"));
  assert_eq!(
    json_body(send(&app, "GET", "/me", &[&session]).await).await["id"],
    Value::Null
  );
}

#[tokio::test]
async fn callbacks_that_do_not_match_the_login_are_refused() {
  let idp = Idp::start().await;
  let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
  let app = app(
    OidcRoutes::new(idp.client().await, sessions.clone()),
    &sessions,
  );
  let start = |return_to: &'static str| {
    let app = app.clone();
    async move {
      let r = send(&app, "GET", &format!("/auth/oidc/login{return_to}"), &[]).await;
      let state = params(&location(&r))["state"].clone();
      (set_cookie(&r, FLOW).unwrap().0, state)
    }
  };
  let status_and_detail = |r: Response| async move {
    let status = r.status();
    (
      status,
      json_body(r).await["detail"]
        .as_str()
        .unwrap_or("")
        .to_owned(),
    )
  };

  // No flow session in this browser (login CSRF: a code injected into another browser).
  let (status, detail) =
    status_and_detail(send(&app, "GET", "/auth/oidc/callback?code=c&state=s", &[]).await).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert!(detail.contains("no login"), "{detail}");

  // A wrong state; the flow session is consumed anyway.
  let (flow, state) = start("").await;
  let wrong = send(
    &app,
    "GET",
    "/auth/oidc/callback?code=c&state=forged",
    &[&flow],
  )
  .await;
  let (status, detail) = status_and_detail(wrong).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert!(detail.contains("state"), "{detail}");
  let right = format!("/auth/oidc/callback?code=c&state={state}");
  assert_eq!(
    send(&app, "GET", &right, &[&flow]).await.status(),
    StatusCode::BAD_REQUEST
  );

  // The provider refused (the user cancelled).
  let (flow, state) = start("").await;
  let uri =
    format!("/auth/oidc/callback?error=access_denied&error_description=cancelled&state={state}");
  let (status, detail) = status_and_detail(send(&app, "GET", &uri, &[&flow]).await).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert!(detail.contains("access_denied (cancelled)"), "{detail}");

  // A mix-up: the callback names another issuer (RFC 9207).
  let (flow, state) = start("").await;
  let uri = format!("/auth/oidc/callback?code=c&state={state}&iss=https://evil.example");
  let (status, detail) = status_and_detail(send(&app, "GET", &uri, &[&flow]).await).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert!(detail.contains("iss"), "{detail}");

  // No code.
  let (flow, state) = start("").await;
  let uri = format!("/auth/oidc/callback?state={state}");
  let (status, detail) = status_and_detail(send(&app, "GET", &uri, &[&flow]).await).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert!(detail.contains("no code"), "{detail}");

  // A nonce from another login is an ID-token rejection (401), not a login.
  let (flow, state) = start("?return_to=//evil.example").await;
  idp
    .answer_tokens(&idp.id_token(json!({"nonce": "another-login"})).await)
    .await;
  let uri = format!("/auth/oidc/callback?code=c&state={state}");
  let r = send(&app, "GET", &uri, &[&flow]).await;
  assert!(set_cookie(&r, SESSION).is_none());
  let (status, detail) = status_and_detail(r).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  assert!(detail.contains("nonce"), "{detail}");
}

#[tokio::test]
async fn an_off_site_return_to_is_ignored_and_routes_are_configurable() {
  let idp = Idp::start().await;
  let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
  let flow_store = MemorySessionStore::new();
  let flow = Sessions::new(flow_store.clone(), OidcRoutes::flow_session_config());
  let routes = OidcRoutes::new(idp.client().await, sessions.clone())
    .flow_sessions(flow)
    .login_path("/sso/start")
    .callback_path("/sso/back")
    .logout_path("/sso/end")
    .after_login("/home")
    .after_logout("/bye")
    .provider_logout(false);
  let app = app(routes, &sessions);

  let r = send(&app, "GET", "/sso/start?return_to=//evil.example", &[]).await;
  assert_eq!(flow_store.len(), 1);
  let flow = set_cookie(&r, FLOW).unwrap().0;
  let q = params(&location(&r));
  // A second login in the same browser replaces the first flow session.
  let r = send(&app, "GET", "/sso/start", &[&flow]).await;
  assert_eq!(flow_store.len(), 1);
  assert_eq!(
    send(
      &app,
      "GET",
      &format!("/sso/back?code=c&state={}", q["state"]),
      &[&flow]
    )
    .await
    .status(),
    StatusCode::BAD_REQUEST
  );
  let flow = set_cookie(&r, FLOW).unwrap().0;
  let q = params(&location(&r));
  idp
    .answer_tokens(&idp.id_token(json!({"nonce": q["nonce"]})).await)
    .await;
  let done = send(
    &app,
    "GET",
    &format!("/sso/back?code=c&state={}", q["state"]),
    &[&flow],
  )
  .await;
  assert_eq!(location(&done), "/home");
  assert!(flow_store.is_empty());
  let session = set_cookie(&done, SESSION).unwrap().0;

  // Provider logout disabled: logout stays in the application.
  let out = send(&app, "POST", "/sso/end", &[&session]).await;
  assert_eq!(location(&out), "/bye");
  // Logout without a session.
  assert_eq!(location(&send(&app, "POST", "/sso/end", &[]).await), "/bye");
}

#[tokio::test]
async fn a_failing_session_store_is_503() {
  let idp = Idp::start().await;
  let store = MemorySessionStore::new();
  let sessions = Sessions::new(store.clone(), SessionConfig::default());
  let flow_store = MemorySessionStore::new();
  let flow = Sessions::new(flow_store.clone(), OidcRoutes::flow_session_config());
  let routes = OidcRoutes::new(idp.client().await, sessions.clone()).flow_sessions(flow);
  let app = app(routes, &sessions);

  flow_store.fail_with(Some("down"));
  let r = send(&app, "GET", "/auth/oidc/login", &[]).await;
  assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
  let body = json_body(r).await;
  assert!(
    !body["detail"].as_str().unwrap().contains("down"),
    "no internals: {body}"
  );
  flow_store.fail_with(None);

  // The flow store fails at the callback.
  let r = send(&app, "GET", "/auth/oidc/login", &[]).await;
  let flow_cookie = set_cookie(&r, FLOW).unwrap().0;
  let q = params(&location(&r));
  flow_store.fail_with(Some("down"));
  let uri = format!("/auth/oidc/callback?code=c&state={}", q["state"]);
  let r = send(&app, "GET", &uri, &[&flow_cookie]).await;
  assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
  flow_store.fail_with(None);

  // The application store fails at login and at logout.
  let r = send(&app, "GET", "/auth/oidc/login", &[]).await;
  let flow_cookie = set_cookie(&r, FLOW).unwrap().0;
  let q = params(&location(&r));
  idp
    .answer_tokens(&idp.id_token(json!({"nonce": q["nonce"]})).await)
    .await;
  store.fail_with(Some("down"));
  let uri = format!("/auth/oidc/callback?code=c&state={}", q["state"]);
  let r = send(&app, "GET", &uri, &[&flow_cookie]).await;
  assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
  let planted = format!("{SESSION}={}", "ab".repeat(32));
  let r = send(&app, "POST", "/auth/oidc/logout", &[&planted]).await;
  assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn id_tokens_are_checked() {
  let idp = Idp::start().await;
  let client = idp.client().await;
  let exchange = |id_token: Option<String>| {
    let (idp, client) = (&idp, client.clone());
    async move {
      let pending = client.begin().pending;
      idp.answer_token(200, tokens(id_token.as_deref())).await;
      run(client.exchange("code", &pending)).await
    }
  };

  // A valid token: the principal, the raw ID token and the provider's tokens.
  let pending = client.begin().pending;
  let good = idp.id_token(json!({"nonce": pending.nonce()})).await;
  idp.answer_tokens(&good).await;
  let login = run(client.exchange("code", &pending)).await.unwrap();
  assert_eq!(login.principal.id(), "alice");
  assert_eq!(login.principal.scheme(), Some(AuthScheme::Session));
  assert_eq!(
    login.principal.claim("email"),
    Some(&json!("alice@example.com"))
  );
  assert_eq!(login.id_token, good);
  assert_eq!(login.tokens.access_token, "access-1");
  assert_eq!(login.tokens.refresh_token.as_deref(), Some("refresh-1"));
  assert_eq!(login.tokens.expires_in, Some(300));
  assert!(!format!("{login:?}").contains(&good));

  let rejected = |r: Result<_, OidcError>| match r {
    Err(OidcError::IdToken(e)) => e,
    other => panic!("expected an ID-token rejection, got {other:?}"),
  };
  // Nonce of another login.
  let t = idp.id_token(json!({"nonce": "other"})).await;
  assert!(
    matches!(rejected(exchange(Some(t)).await), AuthError::InvalidClaims(m) if m.contains("nonce"))
  );
  // Another audience.
  let pending = client.begin().pending;
  let t = idp
    .id_token(json!({"nonce": pending.nonce(), "aud": "api"}))
    .await;
  idp.answer_tokens(&t).await;
  let e = rejected(run(client.exchange("code", &pending)).await);
  assert!(
    matches!(&e, AuthError::InvalidClaims(m) if m.contains("aud")),
    "{e:?}"
  );
  // Expired.
  let pending = client.begin().pending;
  let t = idp
    .id_token(json!({"nonce": pending.nonce(), "exp": 1_000_000_000, "iat": 999_999_000}))
    .await;
  idp.answer_tokens(&t).await;
  assert_eq!(
    rejected(run(client.exchange("code", &pending)).await),
    AuthError::Expired
  );
  // Several audiences without azp.
  let pending = client.begin().pending;
  let t = idp
    .id_token(json!({"nonce": pending.nonce(), "aud": [CLIENT_ID, "api"]}))
    .await;
  idp.answer_tokens(&t).await;
  let e = rejected(run(client.exchange("code", &pending)).await);
  assert!(
    matches!(&e, AuthError::InvalidClaims(m) if m.contains("azp")),
    "{e:?}"
  );
  // Signed by a key the provider never published (after one JWKS refetch).
  let pending = client.begin().pending;
  let forger = common::ec_issuer("forged")
    .issuer(idp.issuer.clone())
    .audience(CLIENT_ID);
  let t = idp
    .id_token_by(&forger, json!({"nonce": pending.nonce()}))
    .await;
  idp.answer_tokens(&t).await;
  assert!(matches!(
    rejected(run(client.exchange("code", &pending)).await),
    AuthError::UnknownKey(_)
  ));
  // No ID token at all.
  assert!(matches!(exchange(None).await, Err(OidcError::Provider(m)) if m.contains("id_token")));

  // The token endpoint refuses the code (used, expired, wrong verifier).
  let pending = client.begin().pending;
  idp
    .answer_token(
      400,
      json!({"error": "invalid_grant", "error_description": "Code not valid"}),
    )
    .await;
  let e = run(client.exchange("code", &pending)).await.unwrap_err();
  assert_eq!(
    e,
    OidcError::Denied {
      error: "invalid_grant".into(),
      description: Some("Code not valid".into())
    }
  );
  // The token endpoint answers garbage.
  idp.answer_token(500, json!("oops")).await;
  let e = run(client.exchange("code", &pending)).await.unwrap_err();
  assert!(matches!(&e, OidcError::Provider(_)), "{e:?}");
  assert_eq!(e.http_status(), 502);
}

#[tokio::test]
async fn claim_mapping_is_configurable() {
  let idp = Idp::start().await;
  let config = idp.config().claims(
    ClaimMapping::default()
      .subject("preferred_username")
      .roles(["realm_access.roles"])
      .tenant(None),
  );
  let client = run(OidcClient::discover(config)).await.unwrap();
  let pending = client.begin().pending;
  let t = idp
    .id_token(json!({
      "nonce": pending.nonce(),
      "preferred_username": "alice.a",
      "realm_access": {"roles": ["auditor"]}
    }))
    .await;
  idp.answer_tokens(&t).await;
  let p = run(client.exchange("code", &pending))
    .await
    .unwrap()
    .principal;
  assert_eq!(
    (p.id(), p.roles(), p.tenant()),
    ("alice.a", &["auditor".to_owned()][..], None)
  );
}

#[tokio::test]
async fn discovery_is_validated() {
  // The document names another issuer (a mix-up or a misconfiguration).
  let idp = Idp::with_discovery(|d| {
    d.insert("issuer".into(), json!("https://other.example"));
  })
  .await;
  let e = run(OidcClient::discover(idp.config())).await.unwrap_err();
  assert!(matches!(&e, OidcError::Discovery(_)), "{e:?}");
  assert_eq!(e.http_status(), 503);

  // The provider says it cannot do PKCE S256.
  let idp = Idp::with_discovery(|d| {
    d.insert("code_challenge_methods_supported".into(), json!(["plain"]));
  })
  .await;
  let e = run(OidcClient::discover(idp.config())).await.unwrap_err();
  assert!(
    matches!(&e, OidcError::Discovery(m) if m.contains("S256")),
    "{e:?}"
  );

  // No end-session endpoint and no PKCE metadata: logins work, logout stays local.
  let idp = Idp::with_discovery(|d| {
    d.remove("end_session_endpoint");
    d.remove("code_challenge_methods_supported");
  })
  .await;
  let client = idp.client().await;
  assert_eq!(client.end_session_endpoint(), None);
  assert_eq!(client.logout_url(None), None);

  // Unreachable, and invalid configuration.
  let nowhere = OidcConfig::new("http://127.0.0.1:9/realms/x", CLIENT_ID, REDIRECT)
    .timeout(Duration::from_secs(2));
  let e = run(OidcClient::discover(nowhere)).await.unwrap_err();
  assert!(matches!(&e, OidcError::Discovery(_)), "{e:?}");
  for bad in [
    OidcConfig::new("not a url", CLIENT_ID, REDIRECT),
    OidcConfig::new(idp.issuer.clone(), CLIENT_ID, "relative/cb"),
    idp.config().post_logout_redirect_url("::"),
  ] {
    let e = run(OidcClient::discover(bad)).await.unwrap_err();
    assert!(matches!(&e, OidcError::Config(_)), "{e:?}");
    assert_eq!(e.http_status(), 500);
  }
}

/// 0.11 M3: a discovery document that redirects is not followed; discovery fails and the
/// redirect target never sees a request.
#[tokio::test]
async fn m3_oidc_discovery_redirect_is_refused() {
  let idp = Idp::start().await;
  let moved = MockServer::start().await;
  Mock::given(method("GET"))
    .and(path("/realms/test/.well-known/openid-configuration"))
    .respond_with(ResponseTemplate::new(302).insert_header(
      "location",
      format!("{}/.well-known/openid-configuration", idp.issuer),
    ))
    .mount(&moved)
    .await;
  let config = OidcConfig::new(format!("{}/realms/test", moved.uri()), CLIENT_ID, REDIRECT);
  let e = run(OidcClient::discover(config)).await.unwrap_err();
  assert!(matches!(&e, OidcError::Discovery(_)), "{e:?}");
  assert!(idp.server.received_requests().await.unwrap().is_empty());
}

/// 0.11 M3: a token endpoint that redirects (307 would resend the code and the client secret)
/// is not followed; the exchange fails and the redirect target never sees a request.
#[tokio::test]
async fn m3_oidc_token_endpoint_redirect_is_refused() {
  let idp = Idp::start().await;
  let client = idp.client().await;
  let pending = client.begin().pending;
  let t = idp.id_token(json!({"nonce": pending.nonce()})).await;
  let elsewhere = MockServer::start().await;
  Mock::given(method("POST"))
    .and(path("/token"))
    .respond_with(ResponseTemplate::new(200).set_body_json(tokens(Some(&t))))
    .mount(&elsewhere)
    .await;
  for status in [302, 307] {
    Mock::given(method("POST"))
      .and(path("/realms/test/token"))
      .respond_with(
        ResponseTemplate::new(status)
          .insert_header("location", format!("{}/token", elsewhere.uri())),
      )
      .up_to_n_times(1)
      .mount(&idp.server)
      .await;
    let e = run(client.exchange("code", &pending)).await.unwrap_err();
    assert!(matches!(&e, OidcError::Provider(_)), "{status}: {e:?}");
  }
  assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

/// 0.11 M3: the issuer and the endpoints the provider publishes must be https (plain http only
/// to loopback, or after the recorded `insecure_allow_http` opt-out).
#[tokio::test]
async fn m3_oidc_endpoints_must_be_https() {
  let plain = OidcConfig::new("http://id.example.com/realms/x", CLIENT_ID, REDIRECT);
  let e = run(OidcClient::discover(plain)).await.unwrap_err();
  assert!(
    matches!(&e, OidcError::Config(m) if m.contains("must be https")),
    "{e:?}"
  );

  let idp = Idp::with_discovery(|d| {
    d.insert(
      "token_endpoint".into(),
      json!("http://id.example.com/token"),
    );
  })
  .await;
  let e = run(OidcClient::discover(idp.config())).await.unwrap_err();
  assert!(
    matches!(&e, OidcError::Discovery(m) if m.contains("token_endpoint")),
    "{e:?}"
  );

  let lax = run(OidcClient::discover(idp.config().insecure_allow_http())).await;
  assert!(lax.is_ok(), "{lax:?}");
  let recorded = id_effect::observability::posture::dangerous_opt_outs();
  assert!(
    recorded
      .iter()
      .any(|(id, _)| *id == id_effect_auth::oidc::INSECURE_ALLOW_HTTP)
  );
}

#[tokio::test]
async fn logout_url_carries_the_hint_when_it_parses() {
  let idp = Idp::start().await;
  let client = idp.client().await;
  assert_eq!(
    client.end_session_endpoint(),
    Some(format!("{}/logout", idp.issuer).as_str())
  );
  let url = client.logout_url(Some("not a jwt")).unwrap();
  assert!(!params(&url).contains_key("id_token_hint"));
  let url = client.logout_url(None).unwrap();
  assert_eq!(params(&url)["client_id"], CLIENT_ID);
}

#[tokio::test]
async fn oidc_client_is_an_oauth_client() {
  let idp = Idp::start().await;
  let client = idp.client().await;
  let oauth: &dyn OAuthClient = &client;
  let url = oauth.authorization_url("caller-state");
  let q = params(&url);
  assert_eq!(q["state"], "caller-state");
  assert!(q["scope"].starts_with("openid"));
  assert!(
    !q.contains_key("code_challenge"),
    "the trait has no verifier to send later"
  );

  idp.answer_tokens(&idp.id_token(json!({})).await).await;
  let (tokens, user) = run(oauth.exchange_code("code")).await.unwrap();
  assert_eq!(tokens.access_token, "access-1");
  assert_eq!(
    (user.sub.as_str(), user.email.as_deref()),
    ("alice", Some("alice@example.com"))
  );
  assert!(
    !idp
      .last_token_request()
      .await
      .0
      .contains_key("code_verifier")
  );

  // A principal id from another claim still reports `sub`; a missing email is `None`.
  let mapped =
    run(OidcClient::discover(idp.config().claims(
      ClaimMapping::default().subject("preferred_username"),
    )))
    .await
    .unwrap();
  let t = idp
    .id_token(json!({"preferred_username": "alice.a", "email": null}))
    .await;
  idp.answer_tokens(&t).await;
  let (_, user) = run(mapped.exchange_code("code")).await.unwrap();
  assert_eq!((user.sub.as_str(), user.email), ("alice", None));

  idp
    .answer_token(400, json!({"error": "invalid_grant"}))
    .await;
  assert_eq!(
    run(oauth.exchange_code("used")).await.unwrap_err(),
    OAuthError::InvalidCode
  );
}
