//! Retry transient AI HTTP failures via [`Schedule`].

use std::time::Duration;

use id_effect::{Effect, Schedule};

use crate::error::AiError;

/// Default retry schedule for transient vendor HTTP errors: exponential backoff from 50 ms, at
/// most 3 retries.
#[inline]
pub fn default_ai_retry_schedule() -> Schedule<(Duration, u64), AiError> {
  Schedule::exponential(Duration::from_millis(50)).intersect(Schedule::recurs(3))
}

/// Run `make` with exponential backoff ([`default_ai_retry_schedule`]) when the effect fails
/// with a transient [`AiError`]; any other failure is returned at once.
pub fn retry_transient_ai_http<A, F>(make: F) -> Effect<A, AiError>
where
  A: Send + 'static,
  F: Fn() -> Effect<A, AiError> + Send + Sync + 'static,
{
  Effect::retry(
    make,
    default_ai_retry_schedule().while_input(AiError::is_transient),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{fail, succeed, suspend};
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

  #[test]
  fn retries_transient_then_succeeds() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    let program = retry_transient_ai_http(move || {
      let c = Arc::clone(&c);
      suspend(move || {
        let n = c.fetch_add(1, Ordering::SeqCst);
        if n < 2 {
          fail(AiError::Transient {
            status: 429,
            detail: "rate".into(),
          })
        } else {
          succeed(42_i32)
        }
      })
    });
    let out = program.block_on().into_result().expect("ok");
    assert_eq!(out, 42);
    assert!(calls.load(Ordering::SeqCst) >= 3);
  }

  #[test]
  fn does_not_retry_unauthorized() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    let program: Effect<i32, AiError> = retry_transient_ai_http(move || {
      let c = Arc::clone(&c);
      suspend(move || {
        c.fetch_add(1, Ordering::SeqCst);
        fail(AiError::Unauthorized)
      })
    });
    let err = program.block_on().into_result().unwrap_err();
    assert_eq!(err.fail_option(), Some(AiError::Unauthorized));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }
}
