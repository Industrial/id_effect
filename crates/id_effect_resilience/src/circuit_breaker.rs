//! Circuit breaker — open after `failure_threshold` failures, probe in half-open after `reset_after`.
//!
//! To retry through a breaker according to a [`Schedule`](id_effect::Schedule), see
//! [`retry_with_breaker`](crate::retry_with_breaker).
//!
//! The cooldown is measured on the running fiber's executor clock, so on a
//! [`TestExecutor`](id_effect::TestExecutor) it is virtual time: a program that sleeps
//! `reset_after` sees the breaker half-open without any real wait.

use std::time::{Duration, Instant};

use id_effect::Or;
use id_effect::{Effect, Ref, fail};

use crate::clock::now;

/// Breaker state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CircuitState {
  /// Normal operation — effects run.
  Closed,
  /// Fail fast without running the inner effect.
  Open,
  /// The cooldown has passed: calls run as probes, and the first outcome closes the breaker
  /// (success) or opens it again (typed failure).
  HalfOpen,
}

/// Error when the breaker is open.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CircuitBreakerError;

/// The breaker's mutable state, updated atomically through one [`Ref`].
#[derive(Clone, Copy, Debug)]
struct Breaker {
  state: CircuitState,
  failures: u32,
  opened_at: Option<Instant>,
}

/// Tracks failures and gates effects. Cloning yields another handle to the same breaker.
#[derive(Clone, Debug)]
pub struct CircuitBreaker {
  cell: Ref<Breaker>,
  failure_threshold: u32,
  reset_after: Duration,
}

impl CircuitBreaker {
  /// A breaker in [`CircuitState::Closed`] that opens after `failure_threshold` consecutive
  /// typed failures and lets probes through `reset_after` later.
  pub fn new(failure_threshold: u32, reset_after: Duration) -> Self {
    Self {
      cell: Ref::new(Breaker {
        state: CircuitState::Closed,
        failures: 0,
        opened_at: None,
      }),
      failure_threshold,
      reset_after,
    }
  }

  /// Runs `effect` when the breaker allows it and records the outcome.
  ///
  /// - Open (cooldown not yet over): fails with `Or::Left(CircuitBreakerError)` without running
  ///   `effect`.
  /// - Success: resets the failure count and closes the breaker.
  /// - Typed failure `e`: counts it (opening the breaker at the threshold, or again from
  ///   half-open) and fails with `Or::Right(e)`.
  ///
  /// Defects and interrupts pass through unrecorded.
  pub fn call<A, E, R>(&self, effect: Effect<A, E, R>) -> Effect<A, Or<CircuitBreakerError, E>, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let (gate, ok, err) = (self.clone(), self.clone(), self.clone());
    now()
      .flat_map(move |t| gate.admit(t))
      .flat_map(move |admitted| {
        if !admitted {
          return fail(Or::Left(CircuitBreakerError));
        }
        effect
          .map_err(Or::Right)
          .flat_map(move |a| ok.record_success().map(move |()| a))
          .catch(move |e| err.record_failure().then(fail(e)))
      })
  }

  /// Current breaker state (an open breaker reads `Open` until a call after the cooldown moves it
  /// to `HalfOpen`).
  pub fn state<E, R>(&self) -> Effect<CircuitState, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.cell.get().map(|b: Breaker| b.state)
  }

  /// Decides whether a call may run at `t`, moving `Open` to `HalfOpen` once the cooldown is over.
  fn admit<E, R>(&self, t: Instant) -> Effect<bool, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let reset_after = self.reset_after;
    self.cell.modify(move |mut b: Breaker| {
      let admitted = match (b.state, b.opened_at) {
        (CircuitState::Closed | CircuitState::HalfOpen, _) => true,
        (CircuitState::Open, Some(at)) if t.saturating_duration_since(at) >= reset_after => {
          b.state = CircuitState::HalfOpen;
          true
        }
        (CircuitState::Open, _) => false,
      };
      (admitted, b)
    })
  }

  fn record_success<E, R>(&self) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.cell.set(Breaker {
      state: CircuitState::Closed,
      failures: 0,
      opened_at: None,
    })
  }

  fn record_failure<E, R>(&self) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let cell = self.cell.clone();
    let threshold = self.failure_threshold;
    now().flat_map(move |t| {
      cell.update(move |mut b: Breaker| {
        b.failures = b.failures.saturating_add(1);
        if b.failures >= threshold || b.state == CircuitState::HalfOpen {
          b.state = CircuitState::Open;
          b.opened_at = Some(t);
        }
        b
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

  use id_effect::{Cause, Exit};
  use id_effect::{die, sleep, succeed, sync};

  use super::*;
  use crate::test_support::{run_ok, run_virtual};

  type Out<A> = Result<A, Or<CircuitBreakerError, &'static str>>;

  /// One call through `cb`, observed as a value.
  fn attempt<A: Send + 'static>(
    cb: &CircuitBreaker,
    eff: Effect<A, &'static str>,
  ) -> Effect<Out<A>, &'static str> {
    cb.call(eff).map(Ok).catch(|e| succeed(Err(e)))
  }

  fn err(e: &'static str) -> Effect<(), &'static str> {
    fail(e)
  }

  fn boom() -> Effect<(), &'static str> {
    fail("boom")
  }

  #[test]
  fn circuit_opens_after_threshold() {
    let cb = CircuitBreaker::new(2, Duration::from_millis(100));
    let prog = attempt(&cb, boom())
      .zip(attempt(&cb, boom()))
      .zip(attempt(&cb, boom()))
      .zip(cb.state());
    let (((a, b), c), state) = run_ok(prog).expect("program");
    assert_eq!(a, Err(Or::Right("boom")));
    assert_eq!(b, Err(Or::Right("boom")));
    assert_eq!(c, Err(Or::Left(CircuitBreakerError)));
    assert_eq!(state, CircuitState::Open);
  }

  #[test]
  fn success_resets_failures_and_closes() {
    let cb = CircuitBreaker::new(2, Duration::from_millis(100));
    let prog = attempt(&cb, boom())
      .then(attempt(&cb, succeed(42)))
      .zip(attempt(&cb, boom()))
      .zip(cb.state());
    let ((ok, after), state) = run_ok(prog).expect("program");
    assert_eq!(ok, Ok(42));
    assert_eq!(
      after,
      Err(Or::Right("boom")),
      "count was reset by the success"
    );
    assert_eq!(state, CircuitState::Closed);
  }

  #[test]
  fn failure_below_threshold_stays_closed() {
    let cb = CircuitBreaker::new(3, Duration::from_secs(60));
    let state = run_ok(attempt(&cb, boom()).then(cb.state())).expect("program");
    assert_eq!(state, CircuitState::Closed);
  }

  #[test]
  fn half_open_recovers_after_virtual_cooldown() {
    let cb = CircuitBreaker::new(1, Duration::from_secs(30));
    let prog = attempt(&cb, boom())
      .then(cb.state())
      .zip(sleep(Duration::from_secs(30)).then(attempt(&cb, succeed(7))))
      .zip(cb.state());
    let (exit, elapsed) = run_virtual(prog);
    let ((opened, probe), state) = exit.into_result().expect("program");
    assert_eq!(opened, CircuitState::Open);
    assert_eq!(probe, Ok(7));
    assert_eq!(state, CircuitState::Closed);
    assert_eq!(
      elapsed,
      Duration::from_secs(30),
      "the cooldown is virtual time"
    );
  }

  #[test]
  fn half_open_failure_reopens_and_restarts_the_cooldown() {
    let cb = CircuitBreaker::new(1, Duration::from_secs(10));
    let prog = attempt(&cb, boom())
      .then(sleep(Duration::from_secs(10)))
      .then(attempt(&cb, err("probe")))
      .zip(cb.state())
      .zip(sleep(Duration::from_secs(5)).then(attempt(&cb, err("x"))))
      .zip(sleep(Duration::from_secs(5)).then(attempt(&cb, err("probe 2"))));
    let (exit, elapsed) = run_virtual(prog);
    let (((probe, reopened), early), late) = exit.into_result().expect("program");
    assert_eq!(probe, Err(Or::Right("probe")));
    assert_eq!(reopened, CircuitState::Open);
    assert_eq!(
      early,
      Err(Or::Left(CircuitBreakerError)),
      "5 s into the new cooldown"
    );
    assert_eq!(late, Err(Or::Right("probe 2")), "10 s after the reopen");
    assert_eq!(elapsed, Duration::from_secs(20));
  }

  #[test]
  fn open_breaker_does_not_run_effect_before_cooldown() {
    let cb = CircuitBreaker::new(1, Duration::from_secs(30));
    let runs = Arc::new(AtomicU32::new(0));
    let r = Arc::clone(&runs);
    let counted = sync(move || {
      r.fetch_add(1, Ordering::SeqCst);
    });
    let prog = attempt(&cb, boom())
      .then(sleep(Duration::from_secs(29)))
      .then(attempt(&cb, counted))
      .zip(cb.state());
    let (out, state) = run_ok(prog).expect("program");
    assert_eq!(out, Err(Or::Left(CircuitBreakerError)));
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(state, CircuitState::Open);
  }

  #[test]
  fn defects_pass_through_unrecorded() {
    let cb = CircuitBreaker::new(1, Duration::from_secs(30));
    let (exit, _) = run_virtual(cb.call(die::<(), &str, _>("crash")));
    assert!(matches!(exit, Exit::Failure(Cause::Die(_))));
    let state = run_ok::<_, &str>(cb.state()).expect("state");
    assert_eq!(state, CircuitState::Closed);
  }

  #[test]
  fn clones_share_one_breaker() {
    let cb = CircuitBreaker::new(1, Duration::from_secs(30));
    let other = cb.clone();
    let state = run_ok(attempt(&cb, boom()).then(other.state())).expect("program");
    assert_eq!(state, CircuitState::Open);
    assert!(format!("{cb:?}").contains("CircuitBreaker"));
  }
}
