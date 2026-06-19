//! Retry transient AI HTTP failures via [`Schedule`].

use std::time::Duration;

use id_effect::kernel::Effect;
use id_effect::{Schedule, ScheduleInput};

use crate::error::AiError;

/// Default retry schedule for transient vendor HTTP errors.
#[inline]
pub fn default_ai_retry_schedule() -> Schedule {
  Schedule::exponential(Duration::from_millis(50)).compose(Schedule::recurs(3))
}

/// Run `make` with exponential backoff when the effect returns a transient [`AiError`].
pub fn retry_transient_ai_http<A, F>(make: F) -> Effect<A, AiError, ()>
where
  A: Send + 'static,
  F: Fn() -> Effect<A, AiError, ()> + Send + Clone + 'static,
{
  Effect::new_async(move |_r: &mut ()| {
    let make = make.clone();
    let mut schedule = default_ai_retry_schedule();
    Box::pin(async move {
      let mut attempt = 0u64;
      loop {
        match make().run(&mut ()).await {
          Ok(value) => return Ok(value),
          Err(err) if err.is_transient() => {
            let input = ScheduleInput { attempt };
            attempt = attempt.saturating_add(1);
            if schedule.next(input).is_none() {
              return Err(err);
            }
            std::thread::sleep(Duration::from_millis(25 * attempt.max(1)));
          }
          Err(err) => return Err(err),
        }
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::kernel::Effect;
  use id_effect::runtime::run_blocking;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

  #[test]
  fn retries_transient_then_succeeds() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    let program = retry_transient_ai_http(move || {
      let c = Arc::clone(&c);
      Effect::new(move |_r| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        if n < 2 {
          Err(AiError::Transient {
            status: 429,
            detail: "rate".into(),
          })
        } else {
          Ok(42_i32)
        }
      })
    });
    let out = run_blocking(program, ()).expect("ok");
    assert_eq!(out, 42);
    assert!(calls.load(Ordering::SeqCst) >= 3);
  }

  #[test]
  fn does_not_retry_unauthorized() {
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    let program: Effect<i32, AiError, ()> = retry_transient_ai_http(move || {
      let c = Arc::clone(&c);
      Effect::new(move |_r| {
        c.fetch_add(1, Ordering::SeqCst);
        Err(AiError::Unauthorized)
      })
    });
    let err = run_blocking(program, ()).unwrap_err();
    assert_eq!(err, AiError::Unauthorized);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }
}
