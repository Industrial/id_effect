//! [`AccountRoutes`]: the account flows as axum routes.

use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http::header::{CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderValue, StatusCode};
use id_effect::{Cause, Effect, Exit, HNil};
use id_effect_axum::route::Problem;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Account, AccountError, Accounts};

/// Default prefix of the routes.
pub const DEFAULT_BASE_PATH: &str = "/auth";

/// The body of `202` answers to `verify/resend` and `password/forgot`, whatever the address.
const ACCEPTED_DETAIL: &str =
  r#"{"detail":"If an account exists for this address, a message is on its way."}"#;

/// The account flows of [`Accounts`] as axum routes under a base path (default
/// [`DEFAULT_BASE_PATH`]). Bodies are JSON; failures answer Problem Details (status from
/// [`AccountError::http_status`], server-side causes not described); every answer is
/// `Cache-Control: no-store`.
///
/// | Route (`POST`) | Body | Answer |
/// |---|---|---|
/// | `{base}/register` | `{"email", "password"}` | **201** `{"id", "email", "email_verified"}`; a verification mail is sent |
/// | `{base}/verify` | `{"token"}` | **200** the verified account |
/// | `{base}/verify/resend` | `{"email"}` | **202**, the same body for any address |
/// | `{base}/password/forgot` | `{"email"}` | **202**, the same body for any address |
/// | `{base}/password/reset` | `{"token", "password"}` | **204**; the account's sessions are revoked |
///
/// The routes do not sign the user in; put a rate limit in front of them.
#[derive(Clone)]
pub struct AccountRoutes {
  accounts: Accounts,
  base: String,
}

impl fmt::Debug for AccountRoutes {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AccountRoutes")
      .field("base", &self.base)
      .finish_non_exhaustive()
  }
}

impl AccountRoutes {
  /// The routes over `accounts`, under [`DEFAULT_BASE_PATH`].
  pub fn new(accounts: Accounts) -> Self {
    Self {
      accounts,
      base: DEFAULT_BASE_PATH.into(),
    }
  }

  /// Serve the routes under `base` (`""` for the root; no trailing slash).
  #[must_use]
  pub fn base_path(mut self, base: impl Into<String>) -> Self {
    self.base = base.into();
    self
  }

  /// The five routes, for `Router::merge` into the application's router.
  pub fn router<S: Clone + Send + Sync + 'static>(self) -> Router<S> {
    let base = self.base.clone();
    Router::new()
      .route(&format!("{base}/register"), post(register))
      .route(&format!("{base}/verify"), post(verify))
      .route(&format!("{base}/verify/resend"), post(resend))
      .route(&format!("{base}/password/forgot"), post(forgot))
      .route(&format!("{base}/password/reset"), post(reset))
      .with_state(Arc::new(self))
  }
}

type Routes = State<Arc<AccountRoutes>>;

#[derive(Deserialize)]
struct Credentials {
  email: String,
  password: String,
}

#[derive(Deserialize)]
struct Email {
  email: String,
}

#[derive(Deserialize)]
struct Token {
  token: String,
}

#[derive(Deserialize)]
struct Reset {
  token: String,
  password: String,
}

async fn register(State(r): Routes, body: Bytes) -> Response {
  let Credentials { email, password } = match parse(&body) {
    Ok(b) => b,
    Err(e) => return bad_body(&e),
  };
  match exec(r.accounts.register(&email, password)).await {
    Ok(account) => account_response(StatusCode::CREATED, &account),
    Err(e) => problem(&e),
  }
}

async fn verify(State(r): Routes, body: Bytes) -> Response {
  let Token { token } = match parse(&body) {
    Ok(b) => b,
    Err(e) => return bad_body(&e),
  };
  match exec(r.accounts.verify_email(&token)).await {
    Ok(account) => account_response(StatusCode::OK, &account),
    Err(e) => problem(&e),
  }
}

async fn resend(State(r): Routes, body: Bytes) -> Response {
  let Email { email } = match parse(&body) {
    Ok(b) => b,
    Err(e) => return bad_body(&e),
  };
  accepted(exec(r.accounts.request_verification(&email)).await)
}

async fn forgot(State(r): Routes, body: Bytes) -> Response {
  let Email { email } = match parse(&body) {
    Ok(b) => b,
    Err(e) => return bad_body(&e),
  };
  accepted(exec(r.accounts.forgot_password(&email)).await)
}

async fn reset(State(r): Routes, body: Bytes) -> Response {
  let Reset { token, password } = match parse(&body) {
    Ok(b) => b,
    Err(e) => return bad_body(&e),
  };
  match exec(r.accounts.reset_password(&token, password)).await {
    Ok(_) => no_store(StatusCode::NO_CONTENT.into_response()),
    Err(e) => problem(&e),
  }
}

/// The uniform answer of the address-only routes: **202** and one fixed body.
fn accepted(outcome: Result<(), AccountError>) -> Response {
  match outcome {
    Ok(()) => json(StatusCode::ACCEPTED, ACCEPTED_DETAIL.to_owned()),
    Err(e) => problem(&e),
  }
}

/// Runs a flow on the request's fiber.
async fn exec<A: Send + 'static>(eff: Effect<A, AccountError>) -> Result<A, AccountError> {
  match id_effect_axum::run_with_env(HNil, move |_| eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(other) => Err(AccountError::Storage(format!(
      "account flow ended abnormally: {other:?}"
    ))),
  }
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, serde_json::Error> {
  serde_json::from_slice(body)
}

fn bad_body(e: &serde_json::Error) -> Response {
  no_store(
    Problem::new(StatusCode::BAD_REQUEST)
      .with_detail(format!("the body is not the expected JSON: {e}"))
      .into_response(),
  )
}

fn account_response(status: StatusCode, account: &Account) -> Response {
  let body = serde_json::json!({
    "id": account.id,
    "email": account.email,
    "email_verified": account.email_verified,
  });
  json(status, body.to_string())
}

fn json(status: StatusCode, body: String) -> Response {
  let mut response = (status, body).into_response();
  response
    .headers_mut()
    .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
  no_store(response)
}

fn no_store(mut response: Response) -> Response {
  response
    .headers_mut()
    .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
  response
}

/// Problem Details for `e`; server-side causes are not described to the client. The error is a
/// response extension (for logging middleware).
fn problem(e: &AccountError) -> Response {
  let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
  let detail = if status.is_server_error() {
    "the request could not be completed; try again later".to_owned()
  } else {
    e.to_string()
  };
  let mut response = no_store(Problem::new(status).with_detail(detail).into_response());
  response.extensions_mut().insert(e.clone());
  response
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn problems_hide_server_side_causes() {
    let r = problem(&AccountError::Storage("secret dsn".into()));
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.headers()[CACHE_CONTROL], "no-store");
    assert_eq!(
      r.extensions().get::<AccountError>(),
      Some(&AccountError::Storage("secret dsn".into()))
    );
    assert_eq!(
      problem(&AccountError::EmailTaken).status(),
      StatusCode::CONFLICT
    );
    assert!(parse::<Email>(b"{").is_err());
    assert!(parse::<Email>(br#"{"email":"a@b.c"}"#).is_ok());
    let ok = accepted(Ok(()));
    assert_eq!(ok.status(), StatusCode::ACCEPTED);
    assert_eq!(ok.headers()[CONTENT_TYPE], "application/json");
  }
}
