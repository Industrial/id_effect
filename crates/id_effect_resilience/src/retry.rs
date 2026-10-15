//! Retry through a [`CircuitBreaker`] according to a [`Schedule`].
//!
//! Each attempt goes through [`CircuitBreaker::call`], so successes and failures are recorded in
//! the breaker. Failures are retried while the [`Schedule`] allows; once the breaker is open the
//! loop stops immediately with [`CircuitBreakerError`] instead of sleeping through more attempts.
//!
//! This is [`Effect::retry`] with the caller's schedule wrapped so that an open breaker ends it:
//! backoff delays are slept on the running fiber's executor clock (virtual time on
//! [`TestExecutor`](id_effect::TestExecutor)).

use id_effect::Or;
use id_effect::{Effect, Schedule};

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerError};

/// Retries `make()` per `schedule`, guarded by `breaker`.
///
/// An effect is one-shot, so the attempt is given as a factory: `make` is called once per
/// attempt, when the attempt starts (as for [`Effect::retry`]). `schedule` is stepped with each
/// typed failure `E`.
///
/// Outcomes:
/// - success — an attempt succeeded (the breaker is closed again);
/// - `Or::Right(e)` — the schedule was exhausted; `e` is the last failure;
/// - `Or::Left(CircuitBreakerError)` — the breaker was (or became) open, so retrying stopped.
///
/// Defects and interrupts are not retried.
///
/// ```
/// use std::time::Duration;
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU32, Ordering};
///
/// use id_effect::Exit;
/// use id_effect::{Effect, Schedule, fail, succeed, suspend};
/// use id_effect_resilience::{CircuitBreaker, retry_with_breaker};
///
/// let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
/// let calls = Arc::new(AtomicU32::new(0));
/// let c = Arc::clone(&calls);
/// // Fails once with "503", then succeeds; retried after 1 ms.
/// let call = move || -> Effect<&'static str, &'static str> {
///   let c = Arc::clone(&c);
///   suspend(move || if c.fetch_add(1, Ordering::SeqCst) == 0 { fail("503") } else { succeed("200") })
/// };
/// let out = retry_with_breaker(call, Schedule::spaced(Duration::from_millis(1)), &breaker);
/// assert!(matches!(out.block_on(), Exit::Success("200")));
/// assert_eq!(calls.load(Ordering::SeqCst), 2);
/// ```
pub fn retry_with_breaker<A, E, R, Out, F>(
  make: F,
  schedule: Schedule<Out, E>,
  breaker: &CircuitBreaker,
) -> Effect<A, Or<CircuitBreakerError, E>, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  Out: Send + 'static,
  F: Fn() -> Effect<A, E, R> + Send + Sync + 'static,
{
  let breaker = breaker.clone();
  Effect::retry(move || breaker.call(make()), until_open(schedule))
}

/// `schedule` over the breaker-guarded error: an open breaker (`Or::Left`) ends it at once, and a
/// typed failure (`Or::Right(e)`) steps `schedule` with `e`.
fn until_open<Out, E>(
  schedule: Schedule<Out, E>,
) -> Schedule<Option<Out>, Or<CircuitBreakerError, E>>
where
  Out: Send + 'static,
  E: Send + 'static,
{
  let first = schedule.clone();
  Schedule::from_fn(
    move || first.start(),
    move |now, input: &Or<CircuitBreakerError, E>, s| match input {
      Or::Left(_) => (s, None, None),
      Or::Right(e) => {
        let (s, out, delay) = schedule.step(now, e, s).into_parts();
        (s, Some(out), delay)
      }
    },
  )
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};
  use std::time::Duration;

  use id_effect::{fail, sleep, succeed, suspend};

  use super::*;
  use crate::circuit_breaker::CircuitState;
  use crate::test_support::{run_ok, run_virtual};

  type Out<A> = Result<A, Or<CircuitBreakerError, &'static str>>;

  /// Factory that fails until attempt `succeed_on` (1-based), counting every run.
  fn flaky(
    runs: &Arc<AtomicU32>,
    succeed_on: u32,
  ) -> impl Fn() -> Effect<u32, &'static str> + Send + Sync + 'static {
    let runs = Arc::clone(runs);
    move || {
      let runs = Arc::clone(&runs);
      suspend(move || {
        let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= succeed_on {
          succeed(n)
        } else {
          fail("flaky")
        }
      })
    }
  }

  fn observed(eff: Effect<u32, Or<CircuitBreakerError, &'static str>>) -> Effect<Out<u32>> {
    eff.map(Ok).catch(|e| succeed(Err(e)))
  }

  fn breaker(threshold: u32) -> CircuitBreaker {
    CircuitBreaker::new(threshold, Duration::from_secs(60))
  }

  #[test]
  fn retries_until_success_and_leaves_breaker_closed() {
    let cb = breaker(5);
    let runs = Arc::new(AtomicU32::new(0));
    let eff = retry_with_breaker(
      flaky(&runs, 3),
      Schedule::spaced(Duration::from_millis(100)),
      &cb,
    );
    let (out, state) = run_ok(observed(eff).zip(cb.state())).expect("program");
    assert_eq!(out, Ok(3));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(state, CircuitState::Closed);
  }

  #[test]
  fn stops_retrying_once_breaker_opens() {
    let cb = breaker(2);
    let runs = Arc::new(AtomicU32::new(0));
    let eff = retry_with_breaker(flaky(&runs, u32::MAX), Schedule::recurs(10), &cb);
    let (out, state) = run_ok(observed(eff).zip(cb.state())).expect("program");
    assert_eq!(out, Err(Or::Left(CircuitBreakerError)));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(state, CircuitState::Open);
  }

  #[test]
  fn open_breaker_skips_the_remaining_backoff() {
    let cb = breaker(2);
    let runs = Arc::new(AtomicU32::new(0));
    let eff = retry_with_breaker(
      flaky(&runs, u32::MAX),
      Schedule::spaced(Duration::from_secs(1)),
      &cb,
    );
    let (exit, elapsed) = run_virtual(observed(eff));
    assert_eq!(
      exit.into_result().ok(),
      Some(Err(Or::Left(CircuitBreakerError)))
    );
    // Attempt 1, 1 s backoff, attempt 2 opens the breaker, attempt 3 is rejected: no 2nd sleep.
    assert_eq!(elapsed, Duration::from_secs(2));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
  }

  #[test]
  fn exhausted_schedule_returns_last_error() {
    let cb = breaker(10);
    let runs = Arc::new(AtomicU32::new(0));
    let eff = retry_with_breaker(flaky(&runs, u32::MAX), Schedule::recurs(2), &cb);
    let (out, state) = run_ok(observed(eff).zip(cb.state())).expect("program");
    assert_eq!(out, Err(Or::Right("flaky")));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(state, CircuitState::Closed);
  }

  #[test]
  fn already_open_breaker_fails_fast_without_running() {
    let cb = breaker(1);
    let runs = Arc::new(AtomicU32::new(0));
    let open = cb.call(fail::<(), &str, _>("boom")).catch(|_| succeed(()));
    let eff = retry_with_breaker(flaky(&runs, 1), Schedule::recurs(3), &cb);
    let out = run_ok(open.then(observed(eff))).expect("program");
    assert_eq!(out, Err(Or::Left(CircuitBreakerError)));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
  }

  #[test]
  fn half_open_probe_success_closes_breaker_after_virtual_cooldown() {
    let cb = breaker(1);
    let runs = Arc::new(AtomicU32::new(0));
    let open = cb.call(fail::<(), &str, _>("boom")).catch(|_| succeed(()));
    let eff = retry_with_breaker(flaky(&runs, 1), Schedule::recurs(3), &cb);
    let prog = open
      .then(sleep(Duration::from_secs(60)))
      .then(observed(eff))
      .zip(cb.state());
    let (exit, elapsed) = run_virtual(prog);
    let (out, state) = exit.into_result().expect("program");
    assert_eq!(out, Ok(1));
    assert_eq!(state, CircuitState::Closed);
    assert_eq!(elapsed, Duration::from_secs(60));
  }

  #[test]
  fn success_between_failures_resets_the_breaker_count() {
    let cb = breaker(2);
    let runs = Arc::new(AtomicU32::new(0));
    let eff = retry_with_breaker(flaky(&runs, 2), Schedule::recurs(3), &cb);
    let once = cb.call(fail::<(), &str, _>("once")).catch(|_| succeed(()));
    let (out, state) = run_ok(observed(eff).zip(once.then(cb.state()))).expect("program");
    assert_eq!(out, Ok(2));
    assert_eq!(state, CircuitState::Closed);
  }

  #[test]
  fn backoff_delays_are_virtual_time() {
    let cb = breaker(10);
    let runs = Arc::new(AtomicU32::new(0));
    let eff = retry_with_breaker(
      flaky(&runs, 3),
      Schedule::exponential(Duration::from_millis(100)),
      &cb,
    );
    let (exit, elapsed) = run_virtual(observed(eff));
    assert_eq!(exit.into_result().ok(), Some(Ok(3)));
    // 100 ms after attempt 1, 200 ms after attempt 2.
    assert_eq!(elapsed, Duration::from_millis(300));
  }
}
