//! Resilience patterns for [`id_effect::next::Effect`] programs.
//!
//! | Item | Role |
//! |--------|------|
//! | [`CircuitBreaker`] | Fail fast after repeated failures |
//! | [`RateLimiter`] | Token-bucket admission control |
//! | [`Bulkhead`] | Limit concurrent in-flight effects (rejects when full) |
//! | [`hedged()`] | Race a primary against a delayed backup ([`Effect::race`](id_effect::next::Effect::race)) |
//! | [`retry_with_breaker`] | Retry per a [`Schedule`](id_effect::next::Schedule) through a breaker |
//!
//! Every constructor is a plain function, and every operation returns a lazy `next` effect,
//! generic over its environment `R`. Time (the breaker cooldown, token refill, backoff and hedge
//! delays) is read from the running fiber's executor clock, so all of it is virtual time on a
//! [`TestExecutor`](id_effect::next::TestExecutor).
//!
//! ```
//! use std::time::Duration;
//!
//! use id_effect::Or;
//! use id_effect::next::{Effect, Schedule, fail};
//! use id_effect_resilience::{CircuitBreaker, CircuitBreakerError, retry_with_breaker};
//!
//! let breaker = CircuitBreaker::new(2, Duration::from_secs(30));
//! // Retry up to 5 times, but the breaker opens after 2 failures and stops the loop.
//! let eff: Effect<u32, _> = retry_with_breaker(|| fail("down"), Schedule::recurs(5), &breaker);
//! let out = eff.block_on().into_result();
//! assert!(matches!(
//!   out,
//!   Err(id_effect::Cause::Fail(Or::Left(CircuitBreakerError)))
//! ));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod bulkhead;
pub mod circuit_breaker;
mod clock;
pub mod hedged;
pub mod rate_limiter;
pub mod retry;

pub use bulkhead::{Bulkhead, BulkheadError};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerError, CircuitState};
pub use hedged::hedged;
pub use rate_limiter::{RateLimitError, RateLimiter};
pub use retry::retry_with_breaker;

#[cfg(test)]
pub(crate) mod test_support {
  //! Runs a root fiber on a [`TestExecutor`], jumping virtual time from timer to timer.

  use std::future::Future;
  use std::pin::pin;
  use std::sync::Arc;
  use std::task::{Context, Poll, Waker};
  use std::time::Duration;

  use id_effect::Exit;
  use id_effect::next::{Effect, Executor, TestExecutor};

  /// Runs `eff` to completion on a fresh `TestExecutor`; returns its exit and the virtual time
  /// that passed.
  pub(crate) fn run_virtual<A, E>(eff: Effect<A, E>) -> (Exit<A, E>, Duration)
  where
    A: Send + 'static,
    E: Send + 'static,
  {
    let exec = TestExecutor::new();
    let start = exec.now();
    let h = eff.fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    while !h.is_done() {
      let deadline = exec
        .next_timer_deadline()
        .expect("root fiber stalled with no timer pending");
      exec.advance(deadline.saturating_duration_since(exec.now()));
    }
    let mut join = pin!(h.join_future());
    let exit = match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
      Poll::Ready(exit) => exit,
      Poll::Pending => panic!("root fiber is not done"),
    };
    (exit, exec.now().saturating_duration_since(start))
  }

  /// The exit of [`run_virtual`] as a `Result` over the typed error (panics on a defect).
  pub(crate) fn run_ok<A, E>(eff: Effect<A, E>) -> Result<A, E>
  where
    A: Send + 'static,
    E: Send + 'static + std::fmt::Debug,
  {
    match run_virtual(eff).0 {
      Exit::Success(a) => Ok(a),
      Exit::Failure(id_effect::Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }
}
