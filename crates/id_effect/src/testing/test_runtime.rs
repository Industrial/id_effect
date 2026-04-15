//! Deterministic test runtime harness helpers.

use crate::runtime::Never;
use crate::{Effect, Exit, TestClock};
use std::cell::Cell;

thread_local! {
  static LEAKED_FIBERS: Cell<usize> = const { Cell::new(0) };
  static UNCLOSED_SCOPES: Cell<usize> = const { Cell::new(0) };
}

fn reset_counters() {
  LEAKED_FIBERS.with(|c| c.set(0));
  UNCLOSED_SCOPES.with(|c| c.set(0));
}

/// Internal hook for tests that need to simulate leaked fibers.
pub fn record_leaked_fiber() {
  LEAKED_FIBERS.with(|c| c.set(c.get().saturating_add(1)));
}

/// Internal hook for tests that need to simulate unclosed scopes.
pub fn record_unclosed_scope() {
  UNCLOSED_SCOPES.with(|c| c.set(c.get().saturating_add(1)));
}

/// Assert that no leaked fibers were recorded in the current test harness run.
pub fn assert_no_leaked_fibers() -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    let leaks = LEAKED_FIBERS.with(|c| c.get());
    assert_eq!(
      leaks, 0,
      "deterministic test harness detected leaked fibers: {leaks}"
    );
    Ok(())
  })
}

/// Assert that no unclosed scopes were recorded in the current test harness run.
pub fn assert_no_unclosed_scopes() -> Effect<(), Never, ()> {
  Effect::new(move |_env| {
    let leaks = UNCLOSED_SCOPES.with(|c| c.get());
    assert_eq!(
      leaks, 0,
      "deterministic test harness detected unclosed scopes: {leaks}"
    );
    Ok(())
  })
}

/// Run an effect in deterministic test mode and return an `Exit` value.
#[inline]
pub fn run_test<A, E, R>(effect: Effect<A, E, R>, env: R) -> Exit<A, E>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  reset_counters();
  let result = crate::runtime::run_blocking(effect, env);
  let _ = crate::runtime::run_blocking(assert_no_leaked_fibers(), ());
  let _ = crate::runtime::run_blocking(assert_no_unclosed_scopes(), ());
  match result {
    Ok(value) => Exit::succeed(value),
    Err(error) => Exit::fail(error),
  }
}

/// Run an effect in deterministic test mode with an explicit test clock.
#[inline]
pub fn run_test_with_clock<A, E, R>(
  effect: Effect<A, E, R>,
  env: R,
  _clock: TestClock,
) -> Exit<A, E>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  run_test(effect, env)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{fail, succeed};
  use rstest::rstest;

  mod run_test {
    use super::*;

    #[test]
    fn run_test_with_success_effect_returns_success_exit() {
      let exit = run_test(succeed::<u32, (), ()>(7), ());
      assert_eq!(exit, Exit::succeed(7));
    }

    #[test]
    fn run_test_with_failure_effect_returns_failure_exit() {
      let exit = run_test(fail::<(), &'static str, ()>("boom"), ());
      assert_eq!(exit, Exit::fail("boom"));
    }

    #[rstest]
    #[case::zero(0u8)]
    #[case::positive(9u8)]
    fn run_test_with_clock_matches_run_test_semantics_for_successful_effect(#[case] value: u8) {
      let effect = succeed::<u8, (), ()>(value);
      let clock = TestClock::new(std::time::Instant::now());
      let exit = run_test_with_clock(effect, (), clock);
      assert_eq!(exit, Exit::succeed(value));
    }
  }

  mod assertions {
    use super::*;

    #[test]
    #[should_panic(expected = "deterministic test harness detected leaked fibers")]
    fn assert_no_leaked_fibers_when_leaked_fiber_recorded_panics() {
      record_leaked_fiber();
      let _ = crate::runtime::run_blocking(assert_no_leaked_fibers(), ());
    }

    #[test]
    #[should_panic(expected = "deterministic test harness detected unclosed scopes")]
    fn assert_no_unclosed_scopes_when_unclosed_scope_recorded_panics() {
      record_unclosed_scope();
      let _ = crate::runtime::run_blocking(assert_no_unclosed_scopes(), ());
    }
  }
}
