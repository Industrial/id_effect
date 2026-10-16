//! [`IntoApiError`]: the HTTP answer of every use-case error. The only place statuses are
//! chosen.
//!
//! | Error | Status | Code |
//! |---|---|---|
//! | [`TodoNotFound`] | `404` | `not_found` |
//! | [`TitleTaken`] | `409` | `conflict` |
//! | [`InvalidTitle`], [`EmptyBatch`] (and a bad id or body) | `422` | `invalid` |
//! | [`StoreError::Unavailable`], [`CircuitBreakerError`] | `503` | `unavailable` |
//! | [`RequestTimeout`] | `504` | `timeout` |
//! | [`StoreError::Internal`] | `500` | `internal` (the detail is not sent) |

use id_effect::Or;
use id_effect_resilience::CircuitBreakerError;

use super::ApiError;
use crate::application::{EmptyBatch, RequestTimeout};
use crate::domain::todo::{InvalidTitle, StoreError, TitleTaken, TodoNotFound};

/// An error with an HTTP answer. A use case's `err!(A | B | ..)` has one when each of its
/// errors does.
pub trait IntoApiError {
  /// The answer to this error.
  fn into_api_error(self) -> ApiError;
}

impl IntoApiError for TodoNotFound {
  fn into_api_error(self) -> ApiError {
    ApiError::not_found(self.to_string())
  }
}

impl IntoApiError for TitleTaken {
  fn into_api_error(self) -> ApiError {
    ApiError::conflict(self.to_string())
  }
}

impl IntoApiError for InvalidTitle {
  fn into_api_error(self) -> ApiError {
    ApiError::invalid(self.to_string())
  }
}

impl IntoApiError for EmptyBatch {
  fn into_api_error(self) -> ApiError {
    ApiError::invalid(self.to_string())
  }
}

impl IntoApiError for StoreError {
  fn into_api_error(self) -> ApiError {
    match self {
      Self::Unavailable(_) => ApiError::unavailable(self.to_string()),
      Self::Internal(_) => ApiError::internal(),
    }
  }
}

impl IntoApiError for RequestTimeout {
  fn into_api_error(self) -> ApiError {
    ApiError::timeout()
  }
}

impl IntoApiError for CircuitBreakerError {
  fn into_api_error(self) -> ApiError {
    ApiError::unavailable("the store is unavailable: the circuit breaker is open")
  }
}

impl<L: IntoApiError, R: IntoApiError> IntoApiError for Or<L, R> {
  fn into_api_error(self) -> ApiError {
    match self {
      Or::Left(left) => left.into_api_error(),
      Or::Right(right) => right.into_api_error(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::application::StoreFailure;
  use crate::domain::todo::{Title, TodoId};

  fn answer(err: impl IntoApiError) -> (u16, &'static str, String) {
    let err = err.into_api_error();
    (err.status.as_u16(), err.code, err.message)
  }

  #[test]
  fn every_error_has_its_status_code_and_message() {
    assert_eq!(
      answer(TodoNotFound(TodoId::new(7))),
      (404, "not_found", "no todo with id 7".into())
    );
    assert_eq!(
      answer(TitleTaken(Title::parse("a").unwrap())),
      (409, "conflict", "a todo titled \"a\" already exists".into())
    );
    assert_eq!(
      answer(InvalidTitle::Empty),
      (422, "invalid", "the title must not be empty".into())
    );
    assert_eq!(
      answer(EmptyBatch),
      (422, "invalid", "the batch must not be empty".into())
    );
    assert_eq!(
      answer(StoreError::Unavailable("down".into())),
      (503, "unavailable", "the store is unavailable: down".into())
    );
    assert_eq!(
      answer(RequestTimeout),
      (504, "timeout", "the request timed out".into())
    );
    assert_eq!(answer(CircuitBreakerError).0, 503);
    assert_eq!(answer(CircuitBreakerError).1, "unavailable");
  }

  #[test]
  fn internal_details_are_not_sent() {
    assert_eq!(
      answer(StoreError::Internal("relation todos: secret".into())),
      (500, "internal", "internal error".into())
    );
  }

  #[test]
  fn an_or_answers_its_side() {
    let timeout: StoreFailure = Or::Left(RequestTimeout);
    assert_eq!(answer(timeout).0, 504);
    let open: StoreFailure = Or::Right(Or::Left(CircuitBreakerError));
    assert_eq!(answer(open).0, 503);
    let internal: StoreFailure = Or::Right(Or::Right(StoreError::Internal("x".into())));
    assert_eq!(answer(internal).0, 500);
  }
}
