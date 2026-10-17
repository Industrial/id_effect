//! Password login, session cookie and logout over a real socket, with `SessionLayer` loading
//! the session into the request's capabilities.
#![cfg(feature = "axum")]

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use id_effect::{DynEnv, Effect, Exit, HNil, succeed};
use id_effect_auth::{
  MemorySessionStore, PasswordHash, PasswordHasher, PasswordParams, Session, SessionConfig,
  SessionLayer, Sessions,
};
use id_effect_axum::{RequestCaps, execute_with_request_caps, run_with_env};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
struct App {
  hasher: PasswordHasher,
  sessions: Sessions,
  users: Arc<HashMap<String, PasswordHash>>,
}

fn cookie_header(headers: &HeaderMap) -> String {
  headers
    .get(COOKIE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
    .to_owned()
}

/// `POST /login` with body `user:password`.
async fn login(State(app): State<App>, headers: HeaderMap, body: String) -> impl IntoResponse {
  let (user, password) = body.split_once(':').unwrap_or((&body, ""));
  let (user, password) = (user.to_owned(), password.to_owned());
  let stored = app.users.get(&user).cloned();
  let presented = app.sessions.session_id(&cookie_header(&headers));
  let sessions = app.sessions.clone();
  let checked = run_with_env(HNil, move |_| {
    app.hasher.verify_or_dummy(password, stored.as_ref())
  })
  .await;
  if !matches!(checked, Exit::Success(v) if v.is_valid()) {
    return (StatusCode::UNAUTHORIZED, [(SET_COOKIE, String::new())]);
  }
  let s2 = sessions.clone();
  match run_with_env(HNil, move |_| s2.login(user, presented.as_ref())).await {
    Exit::Success(session) => (
      StatusCode::OK,
      [(SET_COOKIE, sessions.set_cookie(&session))],
    ),
    Exit::Failure(_) => (
      StatusCode::SERVICE_UNAVAILABLE,
      [(SET_COOKIE, String::new())],
    ),
  }
}

/// `POST /logout`.
async fn logout(State(app): State<App>, headers: HeaderMap) -> impl IntoResponse {
  if let Some(id) = app.sessions.session_id(&cookie_header(&headers)) {
    let sessions = app.sessions.clone();
    let _ = run_with_env(HNil, move |_| sessions.logout(&id)).await;
  }
  (StatusCode::OK, [(SET_COOKIE, app.sessions.clear_cookie())])
}

fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  let who = env
    .try_get::<Session>()
    .map_or_else(|_| "anonymous".to_owned(), |s| s.user_id().to_owned());
  succeed(who)
}

async fn me(caps: RequestCaps) -> axum::response::Response {
  execute_with_request_caps(State(DynEnv::new()), caps, whoami).await
}

async fn serve(app: App) -> std::net::SocketAddr {
  let router = Router::new()
    .route("/login", post(login))
    .route("/logout", post(logout))
    .route("/me", get(me))
    .layer(SessionLayer::new(app.sessions.clone()))
    .with_state(app);
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
  addr
}

/// One HTTP/1.1 request; returns the status line, the `Set-Cookie` value and the body.
async fn request(
  addr: std::net::SocketAddr,
  method: &str,
  path: &str,
  cookie: Option<&str>,
  body: &str,
) -> (String, Option<String>, String) {
  let mut stream = TcpStream::connect(addr).await.unwrap();
  let cookie = cookie
    .map(|c| format!("Cookie: {c}\r\n"))
    .unwrap_or_default();
  let req = format!(
    "{method} {path} HTTP/1.1\r\nHost: test\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
    body.len()
  );
  stream.write_all(req.as_bytes()).await.unwrap();
  let mut raw = String::new();
  stream.read_to_string(&mut raw).await.unwrap();
  let (head, body) = raw.split_once("\r\n\r\n").unwrap();
  let status = head.lines().next().unwrap().to_owned();
  let set_cookie = head
    .lines()
    .find_map(|l| l.strip_prefix("set-cookie: "))
    .filter(|v| !v.is_empty())
    .map(str::to_owned);
  (status, set_cookie, body.to_owned())
}

/// The `name=value` pair of a `Set-Cookie` value, as a browser sends it back.
fn pair(set_cookie: &str) -> &str {
  set_cookie.split(';').next().unwrap()
}

#[tokio::test]
async fn password_login_session_cookie_and_logout_over_http() {
  let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap());
  let stored = match run_with_env(HNil, {
    let hasher = hasher.clone();
    move |_| hasher.hash("s3cret")
  })
  .await
  {
    Exit::Success(h) => h,
    Exit::Failure(c) => panic!("hash failed: {c:?}"),
  };
  let store = MemorySessionStore::new();
  let app = App {
    hasher,
    sessions: Sessions::new(store.clone(), SessionConfig::default()),
    users: Arc::new(HashMap::from([("alice".to_owned(), stored)])),
  };
  let addr = serve(app).await;

  let (status, _, body) = request(addr, "GET", "/me", None, "").await;
  assert!(status.contains("200"), "{status}");
  assert!(body.ends_with("anonymous"), "{body}");

  let (status, cookie, _) = request(addr, "POST", "/login", None, "alice:wrong").await;
  assert!(status.contains("401") && cookie.is_none(), "{status}");
  let (status, _, _) = request(addr, "POST", "/login", None, "mallory:s3cret").await;
  assert!(status.contains("401"), "unknown user: {status}");

  let (status, cookie, _) = request(addr, "POST", "/login", None, "alice:s3cret").await;
  assert!(status.contains("200"), "{status}");
  let cookie = cookie.expect("Set-Cookie on login");
  assert!(
    cookie.contains("HttpOnly") && cookie.contains("Secure"),
    "{cookie}"
  );

  let (_, _, body) = request(addr, "GET", "/me", Some(pair(&cookie)), "").await;
  assert!(body.ends_with("alice"), "{body}");

  // Logging in again with the old cookie rotates the id; the old one stops working.
  let (_, rotated, _) = request(addr, "POST", "/login", Some(pair(&cookie)), "alice:s3cret").await;
  let rotated = rotated.expect("Set-Cookie on second login");
  assert_ne!(pair(&rotated), pair(&cookie));
  let (_, _, body) = request(addr, "GET", "/me", Some(pair(&cookie)), "").await;
  assert!(body.ends_with("anonymous"), "{body}");

  let (_, cleared, _) = request(addr, "POST", "/logout", Some(pair(&rotated)), "").await;
  assert!(cleared.expect("Set-Cookie on logout").contains("Max-Age=0"));
  let (_, _, body) = request(addr, "GET", "/me", Some(pair(&rotated)), "").await;
  assert!(body.ends_with("anonymous"), "{body}");
  assert!(store.is_empty());
}
