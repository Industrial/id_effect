//! Proptest helpers for [`Effect`] and [`Exit`] assertions.
//!
//! Core helpers work without the `proptest` crate. When `proptest` is available as a
//! dev-dependency, enable the `proptest` feature on this crate for strategy builders
//! and `TestCaseError` adapters.

use crate::testing::test_runtime::run_test;
use crate::{Effect, Exit};

/// Run an effect in deterministic test mode (alias for [`run_test`] in property tests).
#[inline]
pub fn run_effect<A, E, R>(effect: Effect<A, E, R>, env: R) -> Exit<A, E>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  run_test(effect, env)
}

/// Extract the success value when the exit succeeded.
#[inline]
pub fn exit_success_value<A, E>(exit: Exit<A, E>) -> Option<A> {
  match exit {
    Exit::Success(value) => Some(value),
    Exit::Failure(_) => None,
  }
}

/// Assert two exits are equal (for unit tests and manual property loops).
#[inline]
pub fn assert_exit_eq<A, E>(left: &Exit<A, E>, right: &Exit<A, E>)
where
  A: std::fmt::Debug + PartialEq,
  E: std::fmt::Debug,
{
  match (left, right) {
    (Exit::Success(l), Exit::Success(r)) => assert_eq!(l, r, "success value mismatch"),
    (Exit::Failure(l), Exit::Failure(r)) => {
      assert_eq!(format!("{l:?}"), format!("{r:?}"), "failure cause mismatch")
    }
    _ => panic!("exit mismatch:\n  left: {left:?}\n right: {right:?}"),
  }
}

/// Assert an exit is [`Exit::Success`] with the expected value.
#[inline]
pub fn assert_exit_success<A, E>(exit: &Exit<A, E>, expected: &A)
where
  A: std::fmt::Debug + PartialEq,
  E: std::fmt::Debug,
{
  match exit {
    Exit::Success(value) if value == expected => {}
    Exit::Success(value) => panic!("success value mismatch: got {value:?}, expected {expected:?}"),
    Exit::Failure(cause) => {
      panic!("expected Exit::Success({expected:?}), got Exit::Failure({cause:?})")
    }
  }
}

#[cfg(feature = "proptest")]
mod proptest_ext {
  use super::*;

  /// Extract the success value from an [`Exit`], rejecting the property case when not success.
  #[inline]
  pub fn success_value<A, E>(exit: Exit<A, E>) -> Result<A, proptest::test_runner::TestCaseError> {
    match exit {
      Exit::Success(value) => Ok(value),
      Exit::Failure(_) => Err(proptest::test_runner::TestCaseError::reject(
        "expected Exit::Success",
      )),
    }
  }

  /// Assert two exits are equal, propagating through `proptest!`.
  #[inline]
  pub fn prop_assert_exit_eq<A, E>(
    left: &Exit<A, E>,
    right: &Exit<A, E>,
  ) -> Result<(), proptest::test_runner::TestCaseError>
  where
    A: std::fmt::Debug + PartialEq,
    E: std::fmt::Debug,
  {
    match (left, right) {
      (Exit::Success(l), Exit::Success(r)) if l == r => Ok(()),
      (Exit::Failure(l), Exit::Failure(r)) if format!("{l:?}") == format!("{r:?}") => Ok(()),
      _ => Err(proptest::test_runner::TestCaseError::fail(format!(
        "exit mismatch:\n  left: {left:?}\n right: {right:?}"
      ))),
    }
  }

  /// Assert an exit is [`Exit::Success`] with the expected value.
  #[inline]
  pub fn prop_assert_exit_success<A, E>(
    exit: &Exit<A, E>,
    expected: &A,
  ) -> Result<(), proptest::test_runner::TestCaseError>
  where
    A: std::fmt::Debug + PartialEq,
    E: std::fmt::Debug,
  {
    match exit {
      Exit::Success(value) if value == expected => Ok(()),
      Exit::Success(value) => Err(proptest::test_runner::TestCaseError::fail(format!(
        "success value mismatch: got {value:?}, expected {expected:?}"
      ))),
      Exit::Failure(cause) => Err(proptest::test_runner::TestCaseError::fail(format!(
        "expected Exit::Success({expected:?}), got Exit::Failure({cause:?})"
      ))),
    }
  }

  /// Build a proptest [`Strategy`] for [`Exit::Success`] from an inner value strategy.
  pub fn arb_exit_success<A: std::fmt::Debug>(
    value: impl proptest::strategy::Strategy<Value = A>,
  ) -> impl proptest::strategy::Strategy<Value = Exit<A, ()>> {
    value.prop_map(Exit::succeed)
  }

  /// Build a proptest [`Strategy`] for [`Exit::Failure`] with [`Cause::Fail`](crate::Cause::Fail).
  pub fn arb_exit_fail<E: std::fmt::Debug>(
    error: impl proptest::strategy::Strategy<Value = E>,
  ) -> impl proptest::strategy::Strategy<Value = Exit<(), E>> {
    error.prop_map(Exit::fail)
  }
}

#[cfg(feature = "proptest")]
pub use proptest_ext::{
  arb_exit_fail, arb_exit_success, prop_assert_exit_eq, prop_assert_exit_success, success_value,
};

#[cfg(all(test, feature = "proptest"))]
mod tests {
  use super::*;
  use crate::{fail, succeed};
  use proptest::prelude::*;

  proptest! {
    #[test]
    fn run_effect_matches_run_test(value in any::<u32>()) {
      let exit: Exit<u32, ()> = run_effect(succeed(value), ());
      prop_assert_exit_success(&exit, &value)?;
    }

    #[test]
    fn success_value_accepts_success(value in any::<i16>()) {
      let exit: Exit<i16, ()> = Exit::succeed(value);
      let got = success_value(exit).expect("success");
      prop_assert_eq!(got, value);
    }

    #[test]
    fn arb_exit_success_generates_success_variants(_value in any::<u8>()) {
      use proptest::strategy::ValueTree;
      let tree = arb_exit_success(any::<u8>()).new_tree(&mut proptest::test_runner::TestRunner::default()).unwrap();
      let exit = tree.current();
      prop_assert!(matches!(exit, Exit::Success(_)));
    }
  }

  #[test]
  fn success_value_rejects_failure() {
    let exit = run_effect(fail::<(), &str, ()>("nope"), ());
    assert!(success_value(exit).is_err());
  }

  #[test]
  fn prop_assert_exit_eq_reports_mismatch() {
    let left = Exit::<i32, ()>::succeed(1);
    let right = Exit::<i32, ()>::succeed(2);
    assert!(prop_assert_exit_eq(&left, &right).is_err());
  }
}

#[cfg(test)]
mod unit_tests {
  use super::*;
  use crate::succeed;

  #[test]
  fn exit_success_value_extracts_success() {
    let exit: Exit<u8, ()> = run_effect(succeed(11u8), ());
    assert_eq!(exit_success_value(exit), Some(11));
  }

  #[test]
  fn assert_exit_success_passes_for_matching_value() {
    let exit = Exit::<&str, ()>::succeed("ok");
    assert_exit_success(&exit, &"ok");
  }
}
