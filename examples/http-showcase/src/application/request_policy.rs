//! [`RequestPolicy`]: how every store call of a request runs.

use std::time::Duration;

use id_effect::{Effect, Or, Schedule};
use id_effect_resilience::{CircuitBreaker, retry_with_breaker};

use super::{RequestTimeout, StoreFailure};
use crate::domain::todo::StoreError;

/// Each store call runs through a shared circuit breaker, its transient failures
/// ([`StoreError::is_transient`]) retried with exponential backoff, the whole call bounded by a
/// timeout (Effect.ts: `Effect.retry(Schedule.exponential(..).pipe(Schedule.intersect(
/// Schedule.recurs(n)), Schedule.whileInput(isTransient)))` and `Effect.timeoutFail`).
///
/// Clones share the breaker: one policy value serves every request.
#[derive(Clone, Debug)]
pub struct RequestPolicy {
  timeout: Duration,
  retry: Schedule<(Duration, u64), StoreError>,
  breaker: CircuitBreaker,
}

impl RequestPolicy {
  /// Retries of a transient store failure (so up to `1 + DEFAULT_RETRIES` attempts).
  pub const DEFAULT_RETRIES: u64 = 2;

  /// Delay before the first retry; it doubles for each further one.
  pub const DEFAULT_BACKOFF: Duration = Duration::from_millis(10);

  /// Consecutive failed attempts that open the circuit breaker.
  pub const BREAKER_THRESHOLD: u32 = 5;

  /// How long an open breaker rejects calls before it lets one through (half-open).
  pub const BREAKER_RESET: Duration = Duration::from_secs(5);

  /// Calls bounded by `timeout`; up to `retries` retries of a transient failure, `backoff`,
  /// `2 * backoff`, .. apart; a fresh breaker ([`Self::BREAKER_THRESHOLD`],
  /// [`Self::BREAKER_RESET`]).
  pub fn new(timeout: Duration, retries: u64, backoff: Duration) -> Self {
    let retry = Schedule::exponential(backoff)
      .intersect(Schedule::recurs(retries))
      .while_input(StoreError::is_transient);
    Self {
      timeout,
      retry,
      breaker: CircuitBreaker::new(Self::BREAKER_THRESHOLD, Self::BREAKER_RESET),
    }
  }

  /// [`Self::new`] with [`Self::DEFAULT_RETRIES`] and [`Self::DEFAULT_BACKOFF`].
  pub fn with_timeout(timeout: Duration) -> Self {
    Self::new(timeout, Self::DEFAULT_RETRIES, Self::DEFAULT_BACKOFF)
  }

  /// The per-call timeout.
  pub fn timeout(&self) -> Duration {
    self.timeout
  }

  /// Runs `attempt()` under this policy. `attempt` is the call's factory: an effect runs once,
  /// so every retry builds a fresh one.
  pub fn run<A: Send + 'static>(
    &self,
    attempt: impl Fn() -> Effect<A, StoreError> + Send + Sync + 'static,
  ) -> Effect<A, StoreFailure> {
    retry_with_breaker(attempt, self.retry.clone(), &self.breaker)
      .map_err(Or::Right)
      .timeout_fail(self.timeout, || Or::Left(RequestTimeout))
  }
}
