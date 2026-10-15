//! [`AppError`]: every way a `/todos` request can fail, and its HTTP answer.
//!
//! | Variant | Status | When |
//! |---|---|---|
//! | [`NotFound`](AppError::NotFound) | `404` | no todo with that id |
//! | [`Conflict`](AppError::Conflict) | `409` | the title is already taken (titles are unique) |
//! | [`Invalid`](AppError::Invalid) | `422` | a bad id, body or title |
//! | [`Unavailable`](AppError::Unavailable) | `503` | the store cannot be reached (transient: retried) |
//! | [`Timeout`](AppError::Timeout) | `504` | the request ran past `REQUEST_TIMEOUT` |
//! | [`Internal`](AppError::Internal) | `500` | any other store failure (the detail is not sent) |
//!
//! Every error answers a JSON body `{"error": "<code>", "message": "<text>"}`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use id_effect_sql::SqlError;
use serde::Serialize;

/// A failed `/todos` request (module docs: the status of each variant).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppError {
  /// No todo has this id.
  NotFound(i64),
  /// The title (empty when unknown) is already used by another todo.
  Conflict(String),
  /// The request is malformed: a bad id, body or title.
  Invalid(String),
  /// The store cannot be reached right now (transient: the request retries it).
  Unavailable(String),
  /// The request did not finish within its timeout.
  Timeout,
  /// Any other store failure.
  Internal(String),
}

/// The JSON body of an error response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
  /// A stable machine-readable code (`not_found`, `conflict`, `invalid`, `unavailable`,
  /// `timeout`, `internal`).
  pub error: &'static str,
  /// A human-readable message.
  pub message: String,
}

impl AppError {
  /// The HTTP status this error answers.
  pub fn status(&self) -> StatusCode {
    match self {
      Self::NotFound(_) => StatusCode::NOT_FOUND,
      Self::Conflict(_) => StatusCode::CONFLICT,
      Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
      Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
      Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
      Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }

  /// Whether retrying may help: only [`Unavailable`](Self::Unavailable), the store being
  /// unreachable (no statement ran). The request's retry schedule stops on anything else.
  pub fn is_transient(&self) -> bool {
    matches!(self, Self::Unavailable(_))
  }

  /// The JSON body of the response. An [`Internal`](Self::Internal) detail stays on the server.
  pub fn body(&self) -> ErrorBody {
    let (error, message) = match self {
      Self::NotFound(id) => ("not_found", format!("no todo with id {id}")),
      Self::Conflict(title) if title.is_empty() => ("conflict", "the title is taken".to_owned()),
      Self::Conflict(title) => (
        "conflict",
        format!("a todo titled {title:?} already exists"),
      ),
      Self::Invalid(why) => ("invalid", why.clone()),
      Self::Unavailable(why) => ("unavailable", format!("the store is unavailable: {why}")),
      Self::Timeout => ("timeout", "the request timed out".to_owned()),
      Self::Internal(_) => ("internal", "internal error".to_owned()),
    };
    ErrorBody { error, message }
  }
}

impl std::fmt::Display for AppError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Internal(detail) => write!(f, "internal: {detail}"),
      other => f.write_str(&other.body().message),
    }
  }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
  fn into_response(self) -> Response {
    (self.status(), Json(self.body())).into_response()
  }
}

/// Store failures: an unreachable store ([`SqlError::NotConnected`]: the pool is closed or timed
/// out) is [`Unavailable`](AppError::Unavailable), a unique-constraint violation is a
/// [`Conflict`](AppError::Conflict), anything else is [`Internal`](AppError::Internal).
impl From<SqlError> for AppError {
  fn from(err: SqlError) -> Self {
    match err {
      SqlError::NotConnected => Self::Unavailable(err.to_string()),
      SqlError::QueryFailed { ref message, .. } if is_unique_violation(message) => {
        Self::Conflict(String::new())
      }
      other => Self::Internal(other.to_string()),
    }
  }
}

/// PostgreSQL's message for SQLSTATE 23505 (`unique_violation`): `id_effect_sql` keeps only the
/// driver message, not the code.
fn is_unique_violation(message: &str) -> bool {
  message.contains("duplicate key value violates unique constraint")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_variant_has_its_status_and_code() {
    let cases = [
      (AppError::NotFound(7), 404, "not_found"),
      (AppError::Conflict("a".into()), 409, "conflict"),
      (AppError::Invalid("bad".into()), 422, "invalid"),
      (AppError::Unavailable("down".into()), 503, "unavailable"),
      (AppError::Timeout, 504, "timeout"),
      (AppError::Internal("secret".into()), 500, "internal"),
    ];
    for (err, status, code) in cases {
      assert_eq!(err.status().as_u16(), status, "{err:?}");
      assert_eq!(err.body().error, code, "{err:?}");
      assert_eq!(err.clone().into_response().status().as_u16(), status);
    }
  }

  #[test]
  fn only_unavailable_is_transient() {
    assert!(AppError::Unavailable("x".into()).is_transient());
    assert!(!AppError::Timeout.is_transient());
    assert!(!AppError::Internal("x".into()).is_transient());
    assert!(!AppError::NotFound(1).is_transient());
  }

  #[test]
  fn internal_details_are_not_sent() {
    let err = AppError::Internal("relation todos: detail".into());
    assert_eq!(err.body().message, "internal error");
    assert!(
      err.to_string().contains("detail"),
      "kept for the server log"
    );
  }

  #[test]
  fn display_uses_the_message() {
    assert_eq!(AppError::NotFound(3).to_string(), "no todo with id 3");
    assert_eq!(AppError::Timeout.to_string(), "the request timed out");
  }

  #[test]
  fn sql_errors_map_to_the_domain() {
    assert!(matches!(
      AppError::from(SqlError::NotConnected),
      AppError::Unavailable(_)
    ));
    let dup = SqlError::QueryFailed {
      sql: String::new(),
      message: "error returned from database: duplicate key value violates unique constraint \
                \"todos_title_key\""
        .into(),
    };
    assert!(matches!(AppError::from(dup), AppError::Conflict(_)));
    let other = SqlError::Decode("bad cell".into());
    assert!(matches!(AppError::from(other), AppError::Internal(_)));
  }
}
