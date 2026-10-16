//! Running an effect to its [`Exit`] under a [`TestExecutor`], and assertions on `Exit` /
//! [`Cause`] (the 0.6 `assert_exit_*` helpers in a 0.8 form).
//!
//! Every assertion is `#[track_caller]`, so a failure points at the test line, and prints the
//! whole exit on a mismatch.

use std::fmt::Debug;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use crate::kernel::ExitKind;
use crate::runtime::Executor;
use crate::testing::TestExecutor;
use crate::{Cause, Effect, Exit};

/// Runs `eff` to its [`Exit`] on a fresh [`TestExecutor`], advancing virtual time from timer to
/// timer until the effect finishes.
///
/// A `sleep(Duration::from_secs(3600))` costs no wall-clock time. The effect runs as a root
/// fiber, so forked children, `race`, `timeout` and schedules behave as on a real runtime.
///
/// # Panics
///
/// If the effect can make no progress: it is not done, no task is queued and no timer is
/// pending (a deadlock, e.g. a `Deferred` nothing completes). An effect that sleeps forever in
/// a loop never returns.
///
/// ```
/// use std::time::Duration;
///
/// use id_effect::{Effect, assert_exit_success, run_test, sleep};
///
/// let slow: Effect<u8> = sleep(Duration::from_secs(3600)).map(|()| 7);
/// assert_exit_success(&run_test(slow), &7);
/// ```
pub fn run_test<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
  run_test_on(&TestExecutor::new(), eff)
}

/// Like [`run_test`], on an existing executor (its virtual clock carries over between runs, so a
/// sequence of effects can share time, a cache TTL or a schedule).
///
/// # Panics
///
/// As [`run_test`].
pub fn run_test_on<A: Send + 'static, E: Send + 'static>(
  exec: &TestExecutor,
  eff: Effect<A, E>,
) -> Exit<A, E> {
  let handle = eff.fork_root(Arc::new(exec.clone()));
  loop {
    exec.run_until_stalled();
    if handle.is_done() {
      break;
    }
    match exec.next_timer_deadline() {
      Some(deadline) => exec.advance(deadline.saturating_duration_since(exec.now())),
      None => panic!(
        "run_test: the effect is suspended with no queued task and no pending timer (deadlock)"
      ),
    }
  }
  let mut join = pin!(handle.join_future());
  match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(exit) => exit,
    Poll::Pending => unreachable!("run_test: a done fiber's exit is ready"),
  }
}

/// Asserts `exit` is `Exit::Success(expected)`.
///
/// ```
/// use id_effect::{Effect, assert_exit_success, run_test, succeed};
///
/// let eff: Effect<i32, String> = succeed(20).map(|n| n + 1);
/// assert_exit_success(&run_test(eff), &21);
/// ```
#[track_caller]
pub fn assert_exit_success<A, E>(exit: &Exit<A, E>, expected: &A)
where
  A: Debug + PartialEq,
  E: Debug,
{
  match exit {
    Exit::Success(value) if value == expected => {}
    Exit::Success(value) => panic!("success value mismatch: got {value:?}, expected {expected:?}"),
    Exit::Failure(cause) => {
      panic!("expected Exit::Success({expected:?}), got Exit::Failure({cause:?})")
    }
  }
}

/// Asserts `exit` failed and its cause holds `Cause::Fail(expected)` (at the top or inside a
/// `Both` / `Then`).
///
/// ```
/// use id_effect::{Effect, assert_exit_fail, fail, run_test};
///
/// let eff: Effect<u8, &str> = fail("boom");
/// assert_exit_fail(&run_test(eff), &"boom");
/// ```
#[track_caller]
pub fn assert_exit_fail<A, E>(exit: &Exit<A, E>, expected: &E)
where
  A: Debug,
  E: Debug + PartialEq,
{
  match exit {
    Exit::Failure(cause) if cause.contains(&|c| matches!(c, Cause::Fail(e) if e == expected)) => {}
    Exit::Failure(cause) => panic!("expected a Cause::Fail({expected:?}), got {cause:?}"),
    Exit::Success(value) => {
      panic!("expected Exit::Failure(Fail({expected:?})), got Exit::Success({value:?})")
    }
  }
}

/// Asserts `exit` died (a panic or `die` somewhere in the cause) with a defect whose message
/// contains `message` (`""` matches any defect).
///
/// ```
/// use id_effect::{Effect, assert_exit_die, run_test, sync};
///
/// let eff: Effect<u8> = sync(|| panic!("kaput"));
/// assert_exit_die(&run_test(eff), "kaput");
/// ```
#[track_caller]
pub fn assert_exit_die<A, E>(exit: &Exit<A, E>, message: &str)
where
  A: Debug,
  E: Debug,
{
  let matches = |c: &Cause<E>| matches!(c, Cause::Die(d) if d.message().contains(message));
  match exit {
    Exit::Failure(cause) if cause.contains(&matches) => {}
    other => panic!("expected a Cause::Die containing {message:?}, got {other:?}"),
  }
}

/// Asserts `exit` was interrupted and nothing failed or died ([`ExitKind::Interrupt`]).
#[track_caller]
pub fn assert_exit_interrupted<A, E>(exit: &Exit<A, E>)
where
  A: Debug,
  E: Debug,
{
  assert_exit_kind(exit, ExitKind::Interrupt);
}

/// Asserts the [`ExitKind`] of `exit` (`Success`, `Failure`, `Die` or `Interrupt`; a composite
/// cause classifies as `Die` if any leaf died, else `Failure` if any leaf failed).
#[track_caller]
pub fn assert_exit_kind<A, E>(exit: &Exit<A, E>, expected: ExitKind)
where
  A: Debug,
  E: Debug,
{
  let got = ExitKind::of(exit);
  assert!(
    got == expected,
    "expected an exit of kind {expected:?}, got {got:?}: {exit:?}"
  );
}

/// Asserts two exits are equal: same success value, or structurally equal causes (a defect
/// compares by message; an interrupt by fiber id).
#[track_caller]
pub fn assert_exit_eq<A, E>(left: &Exit<A, E>, right: &Exit<A, E>)
where
  A: Debug + PartialEq,
  E: Debug + PartialEq,
{
  match (left, right) {
    (Exit::Success(l), Exit::Success(r)) => assert!(
      l == r,
      "success value mismatch:\n  left: {l:?}\n right: {r:?}"
    ),
    (Exit::Failure(l), Exit::Failure(r)) => assert!(
      causes_equal(l, r),
      "failure cause mismatch:\n  left: {l:?}\n right: {r:?}"
    ),
    _ => panic!("exit mismatch:\n  left: {left:?}\n right: {right:?}"),
  }
}

/// Structural equality of exits with only `PartialEq` on `A` and `E` (`Exit`'s own `==` also
/// needs `Eq + Hash`). Only `testing::property` uses it.
#[cfg(feature = "proptest")]
pub(crate) fn exits_equal<A: PartialEq, E: PartialEq>(l: &Exit<A, E>, r: &Exit<A, E>) -> bool {
  match (l, r) {
    (Exit::Success(l), Exit::Success(r)) => l == r,
    (Exit::Failure(l), Exit::Failure(r)) => causes_equal(l, r),
    _ => false,
  }
}

/// Structural equality of causes: defects by message, interrupts by fiber id.
pub(crate) fn causes_equal<E: PartialEq>(l: &Cause<E>, r: &Cause<E>) -> bool {
  match (l, r) {
    (Cause::Fail(l), Cause::Fail(r)) => l == r,
    (Cause::Die(l), Cause::Die(r)) => l == r,
    (Cause::Interrupt(l), Cause::Interrupt(r)) => l == r,
    (Cause::Both(l1, r1), Cause::Both(l2, r2)) | (Cause::Then(l1, r1), Cause::Then(l2, r2)) => {
      causes_equal(l1, l2) && causes_equal(r1, r2)
    }
    _ => false,
  }
}

/// The success value of `exit`; panics with the cause otherwise.
#[track_caller]
pub fn expect_exit_success<A, E: Debug>(exit: Exit<A, E>) -> A {
  match exit {
    Exit::Success(value) => value,
    Exit::Failure(cause) => panic!("expected Exit::Success, got Exit::Failure({cause:?})"),
  }
}

/// The first typed error (`Cause::Fail`, depth-first) of `exit`; panics if it succeeded or only
/// died / was interrupted.
#[track_caller]
pub fn expect_exit_failure<A: Debug, E: Debug>(exit: Exit<A, E>) -> E {
  match exit {
    Exit::Failure(cause) => match first_fail(cause) {
      Ok(e) => e,
      Err(cause) => panic!("expected a Cause::Fail, got {cause:?}"),
    },
    Exit::Success(value) => panic!("expected Exit::Failure, got Exit::Success({value:?})"),
  }
}

fn first_fail<E>(cause: Cause<E>) -> Result<E, Cause<E>> {
  match cause {
    Cause::Fail(e) => Ok(e),
    Cause::Both(l, r) | Cause::Then(l, r) => match first_fail(*l) {
      Ok(e) => Ok(e),
      Err(l) => first_fail(*r).map_err(|r| Cause::Both(Box::new(l), Box::new(r))),
    },
    other => Err(other),
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::{Deferred, Never, fail, sleep, succeed, sync};

  #[test]
  fn run_test_advances_virtual_time_through_sleeps() {
    let exec = TestExecutor::new();
    let start = exec.now();
    let eff: Effect<u8> = sleep(Duration::from_secs(60))
      .then(sleep(Duration::from_secs(30)))
      .map(|()| 3);
    assert_eq!(run_test_on(&exec, eff), Exit::succeed(3));
    assert_eq!(exec.now() - start, Duration::from_secs(90));
  }

  #[test]
  fn run_test_returns_typed_failure() {
    assert_exit_fail(&run_test(fail::<u8, _, _>("e")), &"e");
  }

  #[test]
  fn run_test_turns_panic_into_die() {
    let eff: Effect<u8> = sync(|| panic!("inside"));
    assert_exit_die(&run_test(eff), "inside");
    assert_exit_kind(
      &run_test(sync::<u8, Never, _>(|| panic!("x"))),
      ExitKind::Die,
    );
  }

  #[test]
  #[should_panic(expected = "deadlock")]
  fn run_test_reports_a_deadlock() {
    let d = Deferred::<u8, Never>::new();
    let _ = run_test(d.wait());
  }

  #[test]
  fn timeout_is_decided_in_virtual_time() {
    let slow: Effect<u8> = sleep(Duration::from_secs(60)).map(|()| 1);
    assert_exit_success(&run_test(slow.timeout(Duration::from_secs(5))), &None);
  }

  #[test]
  #[should_panic(expected = "success value mismatch: got 1, expected 2")]
  fn assert_exit_success_reports_wrong_value() {
    assert_exit_success(&Exit::<i32, ()>::succeed(1), &2);
  }

  #[test]
  #[should_panic(expected = "got Exit::Failure")]
  fn assert_exit_success_reports_failure() {
    assert_exit_success(&Exit::<i32, &str>::fail("boom"), &1);
  }

  #[test]
  fn assert_exit_fail_finds_a_nested_fail() {
    let cause = Cause::then(Cause::die("d"), Cause::fail("e"));
    assert_exit_fail(&Exit::<(), &str>::Failure(cause), &"e");
  }

  #[test]
  #[should_panic(expected = "expected a Cause::Fail(\"x\")")]
  fn assert_exit_fail_reports_other_error() {
    assert_exit_fail(&Exit::<(), &str>::fail("y"), &"x");
  }

  #[test]
  #[should_panic(expected = "got Exit::Success")]
  fn assert_exit_fail_reports_success() {
    assert_exit_fail(&Exit::<u8, &str>::succeed(1), &"x");
  }

  #[test]
  #[should_panic(expected = "Cause::Die")]
  fn assert_exit_die_reports_typed_failure() {
    assert_exit_die(&Exit::<u8, &str>::fail("x"), "");
  }

  #[test]
  fn assert_exit_interrupted_accepts_interrupt_only() {
    assert_exit_interrupted(&Exit::<u8, ()>::interrupt(crate::FiberId::new(4)));
  }

  #[test]
  #[should_panic(expected = "expected an exit of kind Interrupt, got Failure")]
  fn assert_exit_interrupted_rejects_failure() {
    assert_exit_interrupted(&Exit::<u8, &str>::fail("x"));
  }

  #[test]
  fn assert_exit_eq_compares_values_and_causes() {
    assert_exit_eq(&Exit::<i32, ()>::succeed(3), &Exit::succeed(3));
    assert_exit_eq(&Exit::<(), &str>::fail("e"), &Exit::fail("e"));
  }

  #[test]
  #[should_panic(expected = "failure cause mismatch")]
  fn assert_exit_eq_reports_cause_mismatch() {
    assert_exit_eq(&Exit::<(), &str>::fail("e"), &Exit::fail("f"));
  }

  #[test]
  #[should_panic(expected = "exit mismatch")]
  fn assert_exit_eq_reports_kind_mismatch() {
    assert_exit_eq(&Exit::<i32, &str>::succeed(1), &Exit::fail("e"));
  }

  #[test]
  fn expect_exit_success_and_failure_extract() {
    assert_eq!(expect_exit_success(run_test(succeed::<u8, (), _>(9))), 9);
    let both = Cause::both(Cause::die("d"), Cause::fail(5u8));
    assert_eq!(expect_exit_failure(Exit::<(), u8>::Failure(both)), 5);
  }

  #[test]
  #[should_panic(expected = "expected a Cause::Fail")]
  fn expect_exit_failure_rejects_die_only() {
    let _ = expect_exit_failure(Exit::<(), u8>::die("d"));
  }

  #[test]
  #[should_panic(expected = "expected Exit::Success")]
  fn expect_exit_success_rejects_failure() {
    let _ = expect_exit_success(Exit::<u8, u8>::fail(1));
  }
}
