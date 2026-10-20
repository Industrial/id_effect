//! Errors: [`ApiError`] for requests (always answered as an RFC 9457 Problem+JSON body) and
//! [`StartupError`] for the service start.

use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect_kit::auth::{AuthError, OidcError, PasswordError, SessionError, TotpError};
use id_effect_kit::authz::AuthzError;
use id_effect_kit::axum::route::{Issue, Location, Problem};
use id_effect_kit::config::ConfigError;
use id_effect_kit::redis::RedisError;
use id_effect_kit::sql::SqlError;
use id_effect_kit::sql::migrate::MigrateError;
use id_effect_kit::workflow::WorkflowError;

/// Why a request failed. Every variant renders as `application/problem+json`.
#[derive(Debug)]
pub enum ApiError {
  /// No principal, or the anonymous one (401).
  Unauthenticated,
  /// The principal belongs to no organisation, so no ledger is in scope (403).
  NoTenant,
  /// A policy denied the request (401 / 403, from [`AuthzError::http_status`]).
  Authz(AuthzError),
  /// Wrong username, password or TOTP code, or a revoked token (401).
  BadCredentials(&'static str),
  /// The input is well-formed but not acceptable (422, with the issues).
  Invalid(Problem),
  /// No such resource in the caller's organisation (404).
  NotFound(&'static str),
  /// The request conflicts with the current state (409).
  Conflict(String),
  /// A database failure; [`SqlError::RowNotFound`] is 404, a stale version 409.
  Sql(SqlError),
  /// The session store failed (503).
  Session(SessionError),
  /// Password hashing failed (500).
  Password(PasswordError),
  /// TOTP verification failed to run (500).
  Totp(TotpError),
  /// Issuing or checking a token failed.
  Auth(AuthError),
  /// Starting, signalling or reading a workflow failed (404 for an unknown instance, 503 when
  /// its store is away).
  Workflow(WorkflowError),
}

impl ApiError {
  /// A 422 naming one body field (`pointer` is a JSON Pointer into the body).
  pub fn invalid(pointer: &str, message: impl Into<String>) -> Self {
    Self::Invalid(
      Problem::new(StatusCode::UNPROCESSABLE_ENTITY)
        .with_detail("the request input failed validation")
        .with_error(Issue::new(Location::Body, pointer, message.into())),
    )
  }
}

impl fmt::Display for ApiError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Unauthenticated => f.write_str("not signed in"),
      Self::NoTenant => f.write_str("the principal belongs to no organisation"),
      Self::Authz(e) => write!(f, "{e}"),
      Self::BadCredentials(why) => f.write_str(why),
      Self::Invalid(p) => write!(f, "invalid input: {:?}", p.detail),
      Self::NotFound(what) => write!(f, "no such {what}"),
      Self::Conflict(why) => f.write_str(why),
      Self::Sql(e) => write!(f, "{e}"),
      Self::Session(e) => write!(f, "{e}"),
      Self::Password(e) => write!(f, "{e}"),
      Self::Totp(e) => write!(f, "{e}"),
      Self::Auth(e) => write!(f, "{e}"),
      Self::Workflow(e) => write!(f, "{e}"),
    }
  }
}

macro_rules! from_error {
  ($($ty:ty => $variant:ident),* $(,)?) => {
    $(impl From<$ty> for ApiError {
      fn from(e: $ty) -> Self {
        Self::$variant(e)
      }
    })*
  };
}

from_error! {
  AuthzError => Authz,
  SqlError => Sql,
  SessionError => Session,
  PasswordError => Password,
  TotpError => Totp,
  AuthError => Auth,
}

/// The SQLSTATE a driver reported in a [`SqlError`] (`"… (SQLSTATE 23505)"`).
pub fn sqlstate(err: &SqlError) -> Option<&str> {
  let message = match err {
    SqlError::QueryFailed { message, .. } | SqlError::TransactionFailed(message) => message,
    _ => return None,
  };
  message.rsplit_once("(SQLSTATE ")?.1.strip_suffix(')')
}

fn problem(status: StatusCode, detail: impl Into<String>) -> Response {
  Problem::new(status).with_detail(detail).into_response()
}

/// What a database failure means to the client: its status and the detail it is told.
fn sql_meaning(err: &SqlError) -> (StatusCode, &'static str) {
  match err {
    SqlError::RowNotFound => (StatusCode::NOT_FOUND, "not found"),
    SqlError::StaleVersion { .. } => (
      StatusCode::CONFLICT,
      "changed by another request since it was read; reload it and retry",
    ),
    other => match sqlstate(other) {
      Some("23505") => (StatusCode::CONFLICT, "already exists"),
      Some("23503") => (
        StatusCode::UNPROCESSABLE_ENTITY,
        "refers to something that does not exist",
      ),
      Some("23514") => (StatusCode::UNPROCESSABLE_ENTITY, "violates a ledger rule"),
      Some("40001" | "40P01") => (
        StatusCode::CONFLICT,
        "lost a conflict with concurrent postings; retry",
      ),
      _ => (StatusCode::INTERNAL_SERVER_ERROR, "database error"),
    },
  }
}

fn sql_response(err: &SqlError) -> Response {
  let (status, detail) = sql_meaning(err);
  problem(status, detail)
}

impl ApiError {
  /// The status and the `detail` of the Problem this error answers with (`into_response`): what
  /// the RPC surface reports as its typed error ([`crate::rpc::LedgerError`]). A server fault
  /// says only `internal error` (or `database error`).
  pub fn status_detail(&self) -> (StatusCode, String) {
    let status =
      |code: u16| StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    match self {
      Self::Unauthenticated => (StatusCode::UNAUTHORIZED, "sign in first".to_owned()),
      Self::NoTenant => (
        StatusCode::FORBIDDEN,
        "the principal belongs to no organisation".to_owned(),
      ),
      Self::Authz(e) => {
        let detail = match e {
          AuthzError::Unauthenticated => "authentication required",
          AuthzError::Denied(_) => "access denied",
          _ => "authorization could not be evaluated",
        };
        (status(e.http_status()), detail.to_owned())
      }
      Self::BadCredentials(why) => (StatusCode::UNAUTHORIZED, (*why).to_owned()),
      Self::Invalid(p) => (p.status, p.detail.clone().unwrap_or_default()),
      Self::NotFound(what) => (StatusCode::NOT_FOUND, format!("no such {what}")),
      Self::Conflict(why) => (StatusCode::CONFLICT, why.clone()),
      Self::Sql(e) => {
        let (status, detail) = sql_meaning(e);
        (status, detail.to_owned())
      }
      Self::Session(_) => (
        StatusCode::SERVICE_UNAVAILABLE,
        "sessions unavailable".to_owned(),
      ),
      Self::Auth(e) => (status(e.http_status()), e.to_string()),
      Self::Workflow(WorkflowError::NotFound(_)) => {
        (StatusCode::NOT_FOUND, "no such closure".to_owned())
      }
      Self::Workflow(WorkflowError::Sql(_) | WorkflowError::Jobs(_)) => (
        StatusCode::SERVICE_UNAVAILABLE,
        "workflows unavailable".to_owned(),
      ),
      Self::Password(_) | Self::Totp(_) | Self::Workflow(_) => (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".to_owned(),
      ),
    }
  }
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    match self {
      Self::Unauthenticated => problem(StatusCode::UNAUTHORIZED, "sign in first"),
      Self::NoTenant => problem(
        StatusCode::FORBIDDEN,
        "the principal belongs to no organisation",
      ),
      Self::Authz(e) => e.into_response(),
      Self::BadCredentials(why) => problem(StatusCode::UNAUTHORIZED, why),
      Self::Invalid(p) => p.into_response(),
      Self::NotFound(what) => problem(StatusCode::NOT_FOUND, format!("no such {what}")),
      Self::Conflict(why) => problem(StatusCode::CONFLICT, why),
      Self::Sql(e) => sql_response(&e),
      Self::Session(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "sessions unavailable"),
      Self::Auth(e) => problem(
        StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        e.to_string(),
      ),
      Self::Password(_) | Self::Totp(_) => {
        problem(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
      }
      Self::Workflow(WorkflowError::NotFound(_)) => {
        problem(StatusCode::NOT_FOUND, "no such closure")
      }
      Self::Workflow(WorkflowError::Sql(_) | WorkflowError::Jobs(_)) => {
        problem(StatusCode::SERVICE_UNAVAILABLE, "workflows unavailable")
      }
      Self::Workflow(_) => problem(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
  }
}

/// Why the service could not start.
#[derive(Debug)]
pub enum StartupError {
  /// The configuration did not load (files, Vault, environment).
  Config(ConfigError),
  /// A database could not be reached, or a statement failed.
  Sql(SqlError),
  /// The migrations could not be applied.
  Migrate(MigrateError),
  /// Redis could not be reached.
  Redis(RedisError),
  /// OpenID Connect discovery failed.
  Oidc(OidcError),
  /// A setting is unusable (e.g. password hashing parameters).
  Settings(String),
}

impl fmt::Display for StartupError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(e) => write!(f, "configuration: {e}"),
      Self::Sql(e) => write!(f, "database: {e}"),
      Self::Migrate(e) => write!(f, "migrations: {e}"),
      Self::Redis(e) => write!(f, "redis: {e}"),
      Self::Oidc(e) => write!(f, "openid connect: {e}"),
      Self::Settings(e) => write!(f, "settings: {e}"),
    }
  }
}

impl std::error::Error for StartupError {}

impl From<ConfigError> for StartupError {
  fn from(e: ConfigError) -> Self {
    Self::Config(e)
  }
}

impl From<SqlError> for StartupError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

impl From<MigrateError> for StartupError {
  fn from(e: MigrateError) -> Self {
    Self::Migrate(e)
  }
}

impl From<RedisError> for StartupError {
  fn from(e: RedisError) -> Self {
    Self::Redis(e)
  }
}

impl From<OidcError> for StartupError {
  fn from(e: OidcError) -> Self {
    Self::Oidc(e)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn status(e: ApiError) -> StatusCode {
    e.into_response().status()
  }

  fn failed(message: &str) -> SqlError {
    SqlError::QueryFailed {
      sql: String::new(),
      message: message.to_owned(),
    }
  }

  #[test]
  fn database_failures_map_to_their_http_meaning() {
    assert_eq!(sqlstate(&failed("dup (SQLSTATE 23505)")), Some("23505"));
    assert_eq!(sqlstate(&failed("no state")), None);
    assert_eq!(sqlstate(&SqlError::RowNotFound), None);
    let cases = [
      (SqlError::RowNotFound, StatusCode::NOT_FOUND),
      (failed("dup (SQLSTATE 23505)"), StatusCode::CONFLICT),
      (
        failed("fk (SQLSTATE 23503)"),
        StatusCode::UNPROCESSABLE_ENTITY,
      ),
      (
        failed("check (SQLSTATE 23514)"),
        StatusCode::UNPROCESSABLE_ENTITY,
      ),
      (failed("lost (SQLSTATE 40001)"), StatusCode::CONFLICT),
      (failed("other"), StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (err, expected) in cases {
      assert_eq!(ApiError::Sql(err.clone()).status_detail().0, expected);
      assert_eq!(status(ApiError::Sql(err)), expected);
    }
  }

  #[test]
  fn every_error_is_a_problem_and_displays() {
    let cases = [
      (ApiError::Unauthenticated, StatusCode::UNAUTHORIZED),
      (ApiError::NoTenant, StatusCode::FORBIDDEN),
      (ApiError::BadCredentials("no"), StatusCode::UNAUTHORIZED),
      (
        ApiError::invalid("/x", "bad"),
        StatusCode::UNPROCESSABLE_ENTITY,
      ),
      (ApiError::NotFound("account"), StatusCode::NOT_FOUND),
      (ApiError::Conflict("taken".into()), StatusCode::CONFLICT),
      (
        ApiError::Session(SessionError::Storage("down".into())),
        StatusCode::SERVICE_UNAVAILABLE,
      ),
      (
        ApiError::Password(PasswordError::Blocking),
        StatusCode::INTERNAL_SERVER_ERROR,
      ),
      (
        ApiError::Totp(TotpError::Storage("down".into())),
        StatusCode::INTERNAL_SERVER_ERROR,
      ),
    ];
    for (err, expected) in cases {
      assert!(!err.to_string().is_empty());
      // The RPC surface reports what the Problem says.
      assert_eq!(err.status_detail().0, expected, "{err}");
      let res = err.into_response();
      assert_eq!(res.status(), expected);
      assert_eq!(res.headers()["content-type"], "application/problem+json");
    }
    // Token and workflow failures: the RPC surface reports the status the Problem has.
    for err in [
      ApiError::Auth(AuthError::Expired),
      ApiError::Workflow(WorkflowError::NotFound("closure-1".into())),
      ApiError::Workflow(WorkflowError::Sql(SqlError::RowNotFound)),
      ApiError::Workflow(WorkflowError::NoJobQueue),
    ] {
      let (status, detail) = err.status_detail();
      assert!(!detail.is_empty());
      assert_eq!(status, err.into_response().status());
    }
    let startup = StartupError::from(SqlError::RowNotFound);
    assert!(startup.to_string().starts_with("database: "));
    assert!(
      StartupError::Settings("x".into())
        .to_string()
        .starts_with("settings: ")
    );
  }
}
