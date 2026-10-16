//! [`ApiError`]: a failed request's HTTP answer.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use super::ErrorBody;

/// A status and an [`ErrorBody`]. Built through its constructors (one per code); which error
/// gets which one is decided by [`IntoApiError`](super::IntoApiError) only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
  /// The HTTP status.
  pub status: StatusCode,
  /// The machine-readable code ([`ErrorBody::error`]).
  pub code: &'static str,
  /// The human-readable message ([`ErrorBody::message`]).
  pub message: String,
}

impl ApiError {
  /// `404 not_found`.
  pub fn not_found(message: impl Into<String>) -> Self {
    Self::new(StatusCode::NOT_FOUND, "not_found", message)
  }

  /// `409 conflict`.
  pub fn conflict(message: impl Into<String>) -> Self {
    Self::new(StatusCode::CONFLICT, "conflict", message)
  }

  /// `422 invalid`: a bad id, body or title.
  pub fn invalid(message: impl Into<String>) -> Self {
    Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid", message)
  }

  /// `503 unavailable`: the store cannot serve the request right now.
  pub fn unavailable(message: impl Into<String>) -> Self {
    Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
  }

  /// `504 timeout`: "the request timed out".
  pub fn timeout() -> Self {
    Self::new(
      StatusCode::GATEWAY_TIMEOUT,
      "timeout",
      "the request timed out",
    )
  }

  /// `500 internal`: "internal error" (the detail stays on the server).
  pub fn internal() -> Self {
    Self::new(
      StatusCode::INTERNAL_SERVER_ERROR,
      "internal",
      "internal error",
    )
  }

  /// The JSON body of the response.
  pub fn body(&self) -> ErrorBody {
    ErrorBody {
      error: self.code,
      message: self.message.clone(),
    }
  }

  fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
    Self {
      status,
      code,
      message: message.into(),
    }
  }
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    let body = ErrorBody {
      error: self.code,
      message: self.message,
    };
    (self.status, Json(body)).into_response()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_constructor_has_its_status_and_code() {
    let cases = [
      (ApiError::not_found("x"), 404, "not_found"),
      (ApiError::conflict("x"), 409, "conflict"),
      (ApiError::invalid("x"), 422, "invalid"),
      (ApiError::unavailable("x"), 503, "unavailable"),
      (ApiError::timeout(), 504, "timeout"),
      (ApiError::internal(), 500, "internal"),
    ];
    for (err, status, code) in cases {
      assert_eq!((err.status.as_u16(), err.body().error), (status, code));
      assert_eq!(err.into_response().status().as_u16(), status);
    }
  }

  #[tokio::test]
  async fn answers_the_json_body() {
    let res = ApiError::not_found("no todo with id 3").into_response();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
      .await
      .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
      json,
      serde_json::json!({"error": "not_found", "message": "no todo with id 3"})
    );
  }
}
