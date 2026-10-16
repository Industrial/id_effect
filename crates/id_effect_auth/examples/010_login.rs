//! Ex 010 — log in with a password, get a JWT, call guarded routes with it; a service calls with
//! an API key. One `Authenticate` layer accepts Basic, bearer and API-key credentials and hands
//! the principal to `id_effect_authz`'s route guards.
//!
//! Run: `cargo run -p id_effect_auth --features axum,jwt --example 010_login`

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use id_effect::{Exit, HNil};
use id_effect_auth::{
  ApiKey, ApiKeys, AuthPrincipal, Authenticate, BasicAuth, JwtIssuer, JwtKey, JwtVerifier,
  MemoryApiKeyStore, MemoryUsers, PasswordHasher, PasswordParams, basic_header,
};
use id_effect_authz::guard::{AuthorizeExt, Guard};
use id_effect_authz::{Principal, authenticated, require_role};
use id_effect_axum::run_with_env;
use serde_json::json;
use tower::ServiceExt;

const SECRET: &[u8] = b"replace with 32+ random bytes from your secret store";

/// `POST /login` (Basic credentials): the authenticated user gets a 15-minute access token.
async fn login(issuer: JwtIssuer, Extension(user): Extension<AuthPrincipal>) -> Response {
  match run_with_env(HNil, move |_| issuer.issue(&user)).await {
    Exit::Success(token) => {
      Json(json!({ "access_token": token, "token_type": "Bearer" })).into_response()
    }
    Exit::Failure(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
  }
}

async fn me(Extension(p): Extension<AuthPrincipal>) -> String {
  format!("{} via {:?} roles={:?}", p.id(), p.scheme(), p.roles())
}

async fn call(
  app: &Router,
  method: &str,
  path: &str,
  auth: Option<(&str, String)>,
) -> (StatusCode, String) {
  let mut req = Request::builder().method(method).uri(path);
  if let Some((name, value)) = auth {
    req = req.header(name, value);
  }
  let res = app
    .clone()
    .oneshot(req.body(Body::empty()).unwrap())
    .await
    .unwrap();
  let status = res.status();
  let body = res.into_body().collect().await.unwrap().to_bytes();
  (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::main]
async fn main() {
  // Users: production stores PHC strings in a table behind `UserLookup`.
  let hasher = PasswordHasher::new(PasswordParams::OWASP);
  let users = MemoryUsers::new();
  let Exit::Success(hash) = run_with_env(HNil, {
    let hasher = hasher.clone();
    move |_| hasher.hash("correct horse battery staple")
  })
  .await
  else {
    panic!("hashing failed")
  };
  users.insert(
    "ada",
    hash,
    AuthPrincipal::new("user-ada").with_role("accountant"),
  );

  // A service account's API key: the plaintext is shown once, the store keeps a digest.
  let keys = MemoryApiKeyStore::new();
  let (key, record) = ApiKey::generate(
    "ledger",
    AuthPrincipal::new("svc-reports").with_role("auditor"),
  )
  .expect("random source");
  keys.insert(record);

  let issuer = JwtIssuer::hs256(SECRET)
    .issuer("https://ledger.example")
    .audience("ledger-api");
  let verifier = JwtVerifier::new([JwtKey::hmac(SECRET)])
    .issuer("https://ledger.example")
    .audience("ledger-api");

  let app = Router::new()
    .route(
      "/login",
      post({
        let issuer = issuer.clone();
        move |user| login(issuer, user)
      })
      .authorize(Guard::new("login", authenticated())),
    )
    .route("/me", get(me).authorize(Guard::new("me", authenticated())))
    .route(
      "/ledger",
      get(|| async { "2026-10-01  opening balance  1000.00" })
        .authorize(Guard::new("ledger.read", require_role("accountant"))),
    )
    .layer(
      Authenticate::bearer_jwt(verifier)
        .with_api_key(ApiKeys::new(keys, "ledger"))
        .with_basic(BasicAuth::new(users, hasher))
        .realm("ledger"),
    );

  let (status, body) = call(
    &app,
    "POST",
    "/login",
    Some(("authorization", basic_header("ada", "wrong"))),
  )
  .await;
  println!("wrong password      -> {status} {body}");
  let (status, body) = call(
    &app,
    "POST",
    "/login",
    Some((
      "authorization",
      basic_header("ada", "correct horse battery staple"),
    )),
  )
  .await;
  println!("login               -> {status}");
  let token = serde_json::from_str::<serde_json::Value>(&body).unwrap()["access_token"]
    .as_str()
    .unwrap()
    .to_owned();

  let bearer = || Some(("authorization", format!("Bearer {token}")));
  println!(
    "GET /me (bearer)    -> {:?}",
    call(&app, "GET", "/me", bearer()).await
  );
  println!(
    "GET /ledger (bearer)-> {:?}",
    call(&app, "GET", "/ledger", bearer()).await
  );
  println!(
    "GET /ledger (none)  -> {:?}",
    call(&app, "GET", "/ledger", None).await.0
  );
  let tampered = Some(("authorization", format!("Bearer {token}x")));
  println!(
    "GET /me (tampered)  -> {:?}",
    call(&app, "GET", "/me", tampered).await.0
  );

  let api_key = || Some(("x-api-key", key.expose().to_owned()));
  println!(
    "GET /me (API key)   -> {:?}",
    call(&app, "GET", "/me", api_key()).await
  );
  println!(
    "GET /ledger (key)   -> {:?}",
    call(&app, "GET", "/ledger", api_key()).await.0
  );
}
