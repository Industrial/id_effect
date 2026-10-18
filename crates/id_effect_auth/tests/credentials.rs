//! `Authenticate` end to end through an axum router: session, bearer JWT, API key and Basic
//! credentials become the request's `CurrentPrincipal`, which `id_effect_authz` guards and
//! effect handlers read (sec.authn #3).
#![cfg(all(feature = "axum", feature = "jwt"))]

use std::convert::Infallible;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, RETRY_AFTER, WWW_AUTHENTICATE};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect::{DynEnv, Effect, Exit, UtcDateTime, WallClock, succeed};
use id_effect_auth::{
  ApiKey, ApiKeys, AuthError, AuthPrincipal, AuthScheme, Authenticate, BasicAuth, JwtIssuer,
  JwtKey, JwtVerifier, MemoryApiKeyStore, MemorySessionStore, MemoryUsers, PasswordHasher,
  PasswordParams, SessionConfig, SessionLayer, Sessions, basic_header,
};
use id_effect_authz::guard::{AuthorizeExt, Guard};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, require_role};
use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps, run_with_env};
use serde_json::Value;
use tower::ServiceExt;

const SECRET: &[u8] = b"an HS256 secret of at least 32 bytes";
const ISSUER: &str = "https://id.example.com";
const AUDIENCE: &str = "ledger";

struct Fixture {
  issuer: JwtIssuer,
  sessions: Sessions,
  users: MemoryUsers,
  key: ApiKey,
  keys: MemoryApiKeyStore,
  auth: Authenticate,
}

/// `who` reads the principal from the effect environment (`RequestCaps`).
fn who(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  let who = env.try_get::<CurrentPrincipal>().map_or_else(
    |_| "<none>".to_owned(),
    |p| {
      if p.is_authenticated() {
        p.id().to_owned()
      } else {
        "anonymous".to_owned()
      }
    },
  );
  succeed(who)
}

fn app(auth: Authenticate) -> Router {
  Router::new()
    .route(
      "/me",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, who)),
    )
    .route(
      "/ledger",
      get(|| async { "entries" }).authorize(Guard::new("ledger.read", require_role("accountant"))),
    )
    .layer(auth)
    .with_state(DynEnv::new())
}

/// Runs `eff` as a request fiber would and returns its value.
async fn eval<A, E>(eff: Effect<A, E>) -> A
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  match run_with_env(id_effect::HNil, move |_| eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("{cause:?}"),
  }
}

async fn fixture() -> Fixture {
  let hasher = PasswordHasher::new(PasswordParams::new(8, 1, 1).unwrap());
  let users = MemoryUsers::new();
  let hash = eval(hasher.hash("pw")).await;
  users.insert(
    "carol",
    hash,
    AuthPrincipal::new("carol").with_role("accountant"),
  );
  let keys = MemoryApiKeyStore::new();
  let (key, record) = ApiKey::generate("ledger", AuthPrincipal::new("svc-billing")).unwrap();
  keys.insert(record);
  let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
  let verifier = JwtVerifier::new([JwtKey::hmac(SECRET)])
    .issuer(ISSUER)
    .audience(AUDIENCE);
  let auth = Authenticate::new()
    .with_session(sessions.clone())
    .with_bearer_jwt(verifier)
    .with_api_key(ApiKeys::new(keys.clone(), "ledger"))
    .with_basic(BasicAuth::new(users.clone(), hasher))
    .realm("ledger");
  Fixture {
    issuer: JwtIssuer::hs256(SECRET).issuer(ISSUER).audience(AUDIENCE),
    sessions,
    users,
    key,
    keys,
    auth,
  }
}

async fn token(issuer: &JwtIssuer, principal: AuthPrincipal) -> String {
  eval(issuer.issue(&principal)).await
}

async fn send(app: &Router, path: &str, headers: &[(&str, String)]) -> Response {
  let mut req = Request::get(path);
  for (k, v) in headers {
    req = req.header(*k, v);
  }
  app
    .clone()
    .oneshot(req.body(Body::empty()).unwrap())
    .await
    .unwrap()
}

async fn text(res: Response) -> String {
  String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

async fn problem(res: Response) -> Value {
  assert_eq!(res.headers()[CONTENT_TYPE], "application/problem+json");
  serde_json::from_str(&text(res).await).unwrap()
}

fn bearer(token: &str) -> (&'static str, String) {
  ("authorization", format!("Bearer {token}"))
}

#[tokio::test]
async fn a_bearer_token_becomes_the_current_principal() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let t = token(
    &f.issuer,
    AuthPrincipal::new("alice").with_role("accountant"),
  )
  .await;
  let res = send(&app, "/me", &[bearer(&t)]).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(text(res).await, "alice");
  let res = send(&app, "/ledger", &[bearer(&t)]).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(text(res).await, "entries");
}

#[tokio::test]
async fn expired_token_is_401() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let long_ago = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_000_000));
  let t = token(
    &f.issuer.clone().with_clock(long_ago),
    AuthPrincipal::new("alice"),
  )
  .await;
  let res = send(&app, "/me", &[bearer(&t)]).await;
  assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  let challenges: Vec<&str> = res
    .headers()
    .get_all(WWW_AUTHENTICATE)
    .iter()
    .map(|v| v.to_str().unwrap())
    .collect();
  assert_eq!(
    challenges[0],
    "Bearer realm=\"ledger\", error=\"invalid_token\", error_description=\"the token has expired\""
  );
  assert!(challenges.contains(&"ApiKey realm=\"ledger\", header=\"x-api-key\""));
  assert!(challenges.contains(&"Basic realm=\"ledger\", charset=\"UTF-8\""));
  assert_eq!(
    res.extensions().get::<AuthError>(),
    Some(&AuthError::Expired)
  );
  let body = problem(res).await;
  assert_eq!(body["status"], 401);
  assert_eq!(body["scheme"], "bearer");
  assert_eq!(body["detail"], "the token has expired");
}

#[tokio::test]
async fn invalid_bearer_tokens_are_401_at_once() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let foreign = token(
    &JwtIssuer::hs256(b"someone else's secret, also 32+ bytes")
      .issuer(ISSUER)
      .audience(AUDIENCE),
    AuthPrincipal::new("mallory").with_role("accountant"),
  )
  .await;
  let wrong_audience = token(
    &JwtIssuer::hs256(SECRET).issuer(ISSUER).audience("billing"),
    AuthPrincipal::new("alice"),
  )
  .await;
  for t in [foreign, wrong_audience, "garbage".to_owned()] {
    let res = send(&app, "/ledger", &[bearer(&t)]).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{t}");
    assert_eq!(problem(res).await["scheme"], "bearer");
  }
}

#[tokio::test]
async fn missing_credentials_are_anonymous_and_the_guard_decides() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let res = send(&app, "/me", &[]).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(text(res).await, "anonymous");
  let res = send(&app, "/ledger", &[]).await;
  assert_eq!(
    res.status(),
    StatusCode::UNAUTHORIZED,
    "the guard asks for a login"
  );
  // An authenticated principal without the role is forbidden by the guard.
  let t = token(&f.issuer, AuthPrincipal::new("bob")).await;
  assert_eq!(
    send(&app, "/ledger", &[bearer(&t)]).await.status(),
    StatusCode::FORBIDDEN
  );
  // An Authorization scheme nobody configured is no credential either.
  let res = send(&app, "/me", &[("authorization", "Digest x".into())]).await;
  assert_eq!(text(res).await, "anonymous");
}

#[tokio::test]
async fn api_keys_authenticate_and_bad_keys_are_401() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let res = send(&app, "/me", &[("x-api-key", f.key.expose().to_owned())]).await;
  assert_eq!(text(res).await, "svc-billing");
  // Change the last character to a different one (a fixed replacement matches it 1 time in 16).
  let key = f.key.expose();
  let last = if key.ends_with('0') { '1' } else { '0' };
  let bad = format!("{}{last}", &key[..key.len() - 1]);
  let res = send(&app, "/me", &[("x-api-key", bad)]).await;
  assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(problem(res).await["scheme"], "api_key");
  f.keys.fail_with(Some("db down"));
  let res = send(&app, "/me", &[("x-api-key", f.key.expose().to_owned())]).await;
  assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
  assert!(res.headers().get(WWW_AUTHENTICATE).is_none());
}

#[tokio::test]
async fn basic_credentials_are_checked_against_the_user_store() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let res = send(
    &app,
    "/ledger",
    &[("authorization", basic_header("carol", "pw"))],
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);
  for (user, pw) in [("carol", "wrong"), ("nobody", "pw")] {
    let res = send(&app, "/me", &[("authorization", basic_header(user, pw))]).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(
      res
        .headers()
        .get_all(WWW_AUTHENTICATE)
        .iter()
        .any(|v| v.to_str().unwrap().starts_with("Basic realm=\"ledger\""))
    );
    assert_eq!(problem(res).await["detail"], "invalid credentials");
  }
  let res = send(&app, "/me", &[("authorization", "Basic %%%".into())]).await;
  assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  f.users.fail_with(Some("db down"));
  let res = send(
    &app,
    "/me",
    &[("authorization", basic_header("carol", "pw"))],
  )
  .await;
  assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// `send` from the client address `peer` (what `into_make_service_with_connect_info` records).
async fn send_from(app: &Router, peer: &str, headers: &[(&str, String)]) -> Response {
  let mut req = Request::get("/me");
  for (k, v) in headers {
    req = req.header(*k, v);
  }
  let mut req = req.body(Body::empty()).unwrap();
  let peer: std::net::SocketAddr = peer.parse().unwrap();
  req
    .extensions_mut()
    .insert(axum::extract::ConnectInfo(peer));
  app.clone().oneshot(req).await.unwrap()
}

#[tokio::test]
async fn h5_login_throttle_backs_off() {
  let f = fixture().await;
  let app = app(f.auth.clone());
  let guess = |user: &str| [("authorization", basic_header(user, "guess"))];
  // Five free failures for carol, from one client.
  for _ in 0..5 {
    let res = send_from(&app, "192.0.2.1:5000", &guess("carol")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  }
  // Then carol is locked: 429 with Retry-After, even with her real password and from
  // another client, and no WWW-Authenticate challenge.
  for (peer, pw) in [("192.0.2.1:5000", "guess"), ("203.0.113.9:6000", "pw")] {
    let res = send_from(&app, peer, &[("authorization", basic_header("carol", pw))]).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
      res.headers()[RETRY_AFTER],
      "1",
      "base delay after the free attempts"
    );
    assert!(res.headers().get(WWW_AUTHENTICATE).is_none());
    assert!(matches!(
      res.extensions().get::<AuthError>(),
      Some(AuthError::TooManyAttempts { .. })
    ));
    let body = problem(res).await;
    assert_eq!(body["status"], 429);
    assert_eq!(body["scheme"], "basic");
  }
  // The client that failed five times is locked for every account too.
  let res = send_from(&app, "192.0.2.1:5000", &guess("nobody")).await;
  assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
  // The delay doubles: after it passes, one more failure locks carol for 2 s.
  std::thread::sleep(std::time::Duration::from_millis(1_100));
  let res = send_from(&app, "203.0.113.9:6000", &guess("carol")).await;
  assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
  let res = send_from(&app, "203.0.113.9:6000", &guess("carol")).await;
  assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
  assert_eq!(res.headers()[RETRY_AFTER], "2");
}

#[tokio::test]
async fn session_cookies_authenticate_first_unless_reordered() {
  let f = fixture().await;
  let mut session = eval(f.sessions.login("dave", None)).await;
  session.set_attr("roles", "accountant");
  eval(f.sessions.save(session.clone())).await;
  let cookie = f
    .sessions
    .set_cookie(&session)
    .split(';')
    .next()
    .unwrap()
    .to_owned();
  let app_default = app(f.auth.clone());
  let res = send(
    &app_default,
    "/ledger",
    &[(COOKIE.as_str(), cookie.clone())],
  )
  .await;
  assert_eq!(res.status(), StatusCode::OK);
  // Session first: a broken bearer token next to a live session is not even looked at.
  let both = [(COOKIE.as_str(), cookie.clone()), bearer("garbage")];
  let res = send(&app_default, "/me", &both).await;
  assert_eq!(text(res).await, "dave");
  let bearer_first = app(
    f.auth
      .clone()
      .order([AuthScheme::Bearer, AuthScheme::Session]),
  );
  assert_eq!(
    send(&bearer_first, "/me", &both).await.status(),
    StatusCode::UNAUTHORIZED
  );
  // A cookie naming no live session is no credential.
  let stale = format!("{}={}", f.sessions.config().cookie.name, "0".repeat(64));
  let res = send(&app_default, "/me", &[(COOKIE.as_str(), stale)]).await;
  assert_eq!(text(res).await, "anonymous");
}

#[tokio::test]
async fn a_session_loaded_by_session_layer_is_used() {
  let f = fixture().await;
  let session = eval(f.sessions.login("erin", None)).await;
  let cookie = f
    .sessions
    .set_cookie(&session)
    .split(';')
    .next()
    .unwrap()
    .to_owned();
  let app = Router::new()
    .route(
      "/me",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, who)),
    )
    .layer(Authenticate::session(f.sessions.clone()))
    .layer(SessionLayer::new(f.sessions.clone()))
    .with_state(DynEnv::new());
  let res = send(&app, "/me", &[(COOKIE.as_str(), cookie)]).await;
  assert_eq!(text(res).await, "erin");
}

#[tokio::test]
async fn a_principal_installed_by_an_outer_layer_is_kept() {
  let f = fixture().await;
  let app = Router::new()
    .route(
      "/me",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, who)),
    )
    .layer(f.auth.clone())
    .layer(CapOverrideLayer::new(|_| {
      Some(CurrentPrincipal::new(BasicPrincipal::new("job-runner")))
    }))
    .with_state(DynEnv::new());
  assert_eq!(text(send(&app, "/me", &[]).await).await, "job-runner");
  let t = token(&f.issuer, AuthPrincipal::new("alice")).await;
  assert_eq!(text(send(&app, "/me", &[bearer(&t)]).await).await, "alice");
}

#[tokio::test]
async fn single_scheme_constructors_and_the_principal_extension() {
  let f = fixture().await;
  let verifier = JwtVerifier::new([JwtKey::hmac(SECRET)])
    .issuer(ISSUER)
    .audience(AUDIENCE);
  let only_bearer = Router::new()
    .route(
      "/scheme",
      get(
        |axum::Extension(p): axum::Extension<AuthPrincipal>| async move {
          p.scheme().map_or("none", AuthScheme::as_str).to_owned()
        },
      ),
    )
    .layer(Authenticate::bearer_jwt(verifier));
  let t = token(&f.issuer, AuthPrincipal::new("alice")).await;
  assert_eq!(
    text(send(&only_bearer, "/scheme", &[bearer(&t)]).await).await,
    "bearer"
  );
  // API keys are not configured here: the header is ignored and the extension is missing.
  let res = send(
    &only_bearer,
    "/scheme",
    &[("x-api-key", f.key.expose().to_owned())],
  )
  .await;
  assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

  let only_keys = app(Authenticate::api_key(ApiKeys::new(
    f.keys.clone(),
    "ledger",
  )));
  let res = send(
    &only_keys,
    "/me",
    &[("x-api-key", f.key.expose().to_owned())],
  )
  .await;
  assert_eq!(text(res).await, "svc-billing");
  let res = send(&only_keys, "/me", &[bearer(&t)]).await;
  assert_eq!(text(res).await, "anonymous");
  let hasher = PasswordHasher::new(PasswordParams::new(8, 1, 1).unwrap());
  let only_basic = app(Authenticate::basic(BasicAuth::new(f.users.clone(), hasher)));
  let res = send(
    &only_basic,
    "/me",
    &[(AUTHORIZATION.as_str(), basic_header("carol", "pw"))],
  )
  .await;
  assert_eq!(text(res).await, "carol");
  assert!(format!("{only_basic:?}").contains("Router"));
}
