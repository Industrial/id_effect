//! Property testing on [`proptest`](https://docs.rs/proptest) (feature `proptest`).
//!
//! proptest generates the cases, shrinks a failing one to a minimal input and persists its seed
//! under `proptest-regressions/` so the next run replays it first. This module adds what is
//! specific to id_effect:
//!
//! - **Effect-aware properties** — [`prop_check`] and [`prop_run`] run an [`Effect`] to its exit
//!   under a [`TestExecutor`](crate::TestExecutor) (virtual time, see
//!   [`run_test`](crate::run_test)) and turn the outcome into a `TestCaseError`; [`prop_sync`]
//!   lets `prop_assert!` run inside an effect pipeline.
//! - **Exit assertions** — [`prop_assert_exit_success`], [`prop_assert_exit_fail`],
//!   [`prop_assert_exit_eq`]: the `#[track_caller]` panicking forms in
//!   [`testing`](crate::testing), returning `Err` instead so proptest shrinks.
//! - **Generators from schemas** — [`from_schema`] / [`arb_schema`] yield only values a
//!   [`Schema`](crate::Schema) accepts (refinements, brands, filters included);
//!   [`arb_unknown`] / [`unknown_object`] build JSON-shaped wires; [`prop_assert_roundtrip`].
//! - **Generators for framework types** — [`arb_duration`], [`arb_chunk`], [`arb_fiber_id`],
//!   [`arb_defect`], [`arb_cause`], [`arb_exit`], [`arb_hcons`] / [`arb_hnil`].
//! - **Stateful (model-based) testing** — [`StateMachine`], [`run_state_machine`],
//!   [`check_commands`]: random command sequences run against a model and against the real
//!   system (as effects on one `TestExecutor`), shrunk to a minimal failing sequence.
//!
//! ```
//! use id_effect::testing::property::{arb_chunk, prop_check, prop_sync};
//! use id_effect::{Chunk, Effect, succeed};
//! use proptest::prelude::*;
//!
//! // In a test file: `proptest! { #[test] fn chunk_map_preserves_len(chunk in ...) { ... } }`.
//! proptest!(|(chunk in arb_chunk(any::<i32>(), 0..32))| {
//!   let len = chunk.len();
//!   let eff: Effect<Chunk<i64>, TestCaseError> = succeed(chunk).map(|c| c.map(i64::from));
//!   let mapped = prop_check(eff.flat_map(move |c| prop_sync(move || {
//!     prop_assert_eq!(c.len(), len);
//!     Ok(c)
//!   })))?;
//!   prop_assert_eq!(mapped.len(), len);
//! });
//! ```

mod arb;
mod schema;
mod state;

use std::fmt::Debug;

use proptest::test_runner::TestCaseError;

pub use arb::*;
pub use schema::*;
pub use state::*;

use crate::testing::assert::exits_equal;
use crate::testing::{TestExecutor, run_test_on};
use crate::{Cause, Effect, Exit, fail, succeed, suspend};

/// Runs `eff` under a fresh [`TestExecutor`] (as [`run_test`](crate::run_test)) and returns its
/// value; a typed `TestCaseError` failure (e.g. from [`prop_sync`]) is returned as is, a defect
/// or interrupt fails the case.
pub fn prop_check<A: Send + 'static>(eff: Effect<A, TestCaseError>) -> Result<A, TestCaseError> {
  prop_check_on(&TestExecutor::new(), eff)
}

/// [`prop_check`] on an existing executor (shared virtual clock).
pub fn prop_check_on<A: Send + 'static>(
  exec: &TestExecutor,
  eff: Effect<A, TestCaseError>,
) -> Result<A, TestCaseError> {
  match run_test_on(exec, eff) {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(cause) => Err(TestCaseError::fail(format!(
      "effect did not succeed: {}",
      cause_text(&cause)
    ))),
  }
}

/// Runs `eff` under a fresh [`TestExecutor`] and returns its value; any failure (typed error,
/// defect or interrupt) fails the case with the cause.
pub fn prop_run<A, E>(eff: Effect<A, E>) -> Result<A, TestCaseError>
where
  A: Send + 'static,
  E: Debug + Send + 'static,
{
  match crate::run_test(eff) {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(TestCaseError::fail(format!(
      "effect did not succeed: {cause:?}"
    ))),
  }
}

/// An effect that runs `f` when executed and fails with its `TestCaseError`, so `prop_assert!`
/// and `?` work inside an effect pipeline checked by [`prop_check`].
pub fn prop_sync<A: Send + 'static>(
  f: impl FnOnce() -> Result<A, TestCaseError> + Send + 'static,
) -> Effect<A, TestCaseError> {
  suspend(move || match f() {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  })
}

/// `Ok` if `exit` is `Exit::Success(expected)`.
pub fn prop_assert_exit_success<A, E>(exit: &Exit<A, E>, expected: &A) -> Result<(), TestCaseError>
where
  A: Debug + PartialEq,
  E: Debug,
{
  match exit {
    Exit::Success(value) if value == expected => Ok(()),
    Exit::Success(value) => Err(TestCaseError::fail(format!(
      "success value mismatch: got {value:?}, expected {expected:?}"
    ))),
    Exit::Failure(cause) => Err(TestCaseError::fail(format!(
      "expected Exit::Success({expected:?}), got Exit::Failure({cause:?})"
    ))),
  }
}

/// `Ok` if `exit` failed with a cause holding `Cause::Fail(expected)`.
pub fn prop_assert_exit_fail<A, E>(exit: &Exit<A, E>, expected: &E) -> Result<(), TestCaseError>
where
  A: Debug,
  E: Debug + PartialEq,
{
  match exit {
    Exit::Failure(cause) if cause.contains(&|c| matches!(c, Cause::Fail(e) if e == expected)) => {
      Ok(())
    }
    other => Err(TestCaseError::fail(format!(
      "expected a Cause::Fail({expected:?}), got {other:?}"
    ))),
  }
}

/// `Ok` if the two exits are equal (success values, or structurally equal causes).
pub fn prop_assert_exit_eq<A, E>(left: &Exit<A, E>, right: &Exit<A, E>) -> Result<(), TestCaseError>
where
  A: Debug + PartialEq,
  E: Debug + PartialEq,
{
  if exits_equal(left, right) {
    Ok(())
  } else {
    Err(TestCaseError::fail(format!(
      "exit mismatch:\n  left: {left:?}\n right: {right:?}"
    )))
  }
}

/// A cause whose typed errors are `TestCaseError`s, as text.
fn cause_text(cause: &Cause<TestCaseError>) -> String {
  match cause {
    Cause::Fail(e) => format!("Fail({e})"),
    Cause::Die(d) => format!("Die({d})"),
    Cause::Interrupt(id) => format!("Interrupt({id:?})"),
    Cause::Both(l, r) => format!("Both({}, {})", cause_text(l), cause_text(r)),
    Cause::Then(l, r) => format!("Then({}, {})", cause_text(l), cause_text(r)),
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use proptest::prelude::*;

  use super::*;
  use crate::sleep;

  proptest! {
    #[test]
    fn prop_check_returns_the_value_after_virtual_sleeps(n in any::<u32>(), secs in 0u64..10_000) {
      let eff: Effect<u32, TestCaseError> = sleep(Duration::from_secs(secs)).map(move |()| n);
      prop_assert_eq!(prop_check(eff)?, n);
    }

    #[test]
    fn prop_run_matches_run_test(value in any::<i16>()) {
      prop_assert_eq!(prop_run(succeed::<i16, (), _>(value))?, value);
    }

    #[test]
    fn prop_assert_exit_success_accepts_generated_success(value in any::<u8>()) {
      prop_assert_exit_success(&Exit::<u8, ()>::succeed(value), &value)?;
    }
  }

  #[test]
  fn prop_check_passes_a_typed_test_case_error_through() {
    let eff = prop_sync::<()>(|| Err(TestCaseError::fail("inner reason")));
    let err = prop_check(eff).unwrap_err();
    assert!(err.to_string().contains("inner reason"), "{err}");
    assert!(matches!(err, TestCaseError::Fail(_)));
    let rejected = prop_check(prop_sync::<()>(|| Err(TestCaseError::reject("skip")))).unwrap_err();
    assert!(matches!(rejected, TestCaseError::Reject(_)));
  }

  #[test]
  fn prop_check_turns_a_defect_into_a_failure() {
    let eff: Effect<u8, TestCaseError> = crate::sync(|| panic!("kaput"));
    let err = prop_check(eff).unwrap_err();
    assert!(err.to_string().contains("Die(kaput)"), "{err}");
  }

  #[test]
  fn prop_run_reports_the_cause() {
    let err = prop_run(fail::<u8, &str, _>("nope")).unwrap_err();
    assert!(err.to_string().contains("Fail(\"nope\")"), "{err}");
  }

  #[test]
  fn prop_exit_assertions_report_each_mismatch() {
    assert!(prop_assert_exit_success(&Exit::<i32, ()>::succeed(1), &2).is_err());
    assert!(prop_assert_exit_success(&Exit::<i32, &str>::fail("x"), &1).is_err());
    assert!(prop_assert_exit_fail(&Exit::<(), &str>::fail("x"), &"x").is_ok());
    assert!(prop_assert_exit_fail(&Exit::<(), &str>::fail("x"), &"y").is_err());
    assert!(prop_assert_exit_eq(&Exit::<i32, ()>::succeed(3), &Exit::succeed(3)).is_ok());
    assert!(prop_assert_exit_eq(&Exit::<i32, ()>::succeed(3), &Exit::succeed(4)).is_err());
  }

  #[test]
  fn cause_text_renders_composites() {
    let c = Cause::then(
      Cause::fail(TestCaseError::fail("a")),
      Cause::both(Cause::die("d"), Cause::interrupt(crate::FiberId::new(1))),
    );
    let text = cause_text(&c);
    assert!(text.starts_with("Then(Fail("), "{text}");
    assert!(text.contains("Both(Die(d), Interrupt("), "{text}");
  }
}
