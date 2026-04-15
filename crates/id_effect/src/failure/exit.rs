//! [`Exit`] — terminal outcome of an effect execution.
//!
//! [`Exit`] uses structural equality for its payload and cause. When `A` and `E` implement
//! [`Hash`](std::hash::Hash) (and the standard equality traits), the exit value participates in
//! [`crate::schema::data::EffectData`] semantics through the usual blanket impl.

use super::cause::Cause;
use crate::Matcher;
use crate::foundation::either::Either;
use crate::runtime::FiberId;

/// Final outcome of an effect: either a success value or a structured failure [`Cause`].
#[derive(Clone, Debug, crate::EffectData)]
pub enum Exit<A, E> {
  /// Completed successfully with `A`.
  Success(A),
  /// Completed with interrupt, defect, or typed failure.
  Failure(Cause<E>),
}

impl<A, E> Exit<A, E> {
  /// Wraps `value` in [`Exit::Success`].
  #[inline]
  pub fn succeed(value: A) -> Self {
    Self::Success(value)
  }

  /// Wraps `error` in [`Exit::Failure`] as [`Cause::Fail`].
  #[inline]
  pub fn fail(error: E) -> Self {
    Self::Failure(Cause::fail(error))
  }

  /// Defect exit — [`Exit::Failure`] with [`Cause::Die`].
  #[inline]
  pub fn die(message: impl Into<String>) -> Self {
    Self::Failure(Cause::die(message))
  }

  /// Cancellation-style exit — [`Exit::Failure`] with [`Cause::Interrupt`].
  #[inline]
  pub fn interrupt(fiber_id: FiberId) -> Self {
    Self::Failure(Cause::interrupt(fiber_id))
  }

  /// Maps a successful value; leaves failure causes unchanged.
  #[inline]
  pub fn map<B, F>(self, map: F) -> Exit<B, E>
  where
    F: FnOnce(A) -> B,
  {
    match self {
      Exit::Success(value) => Exit::Success(map(value)),
      Exit::Failure(cause) => Exit::Failure(cause),
    }
  }

  /// Maps only the typed error inside [`Cause::Fail`]; die and interrupt causes are preserved.
  #[inline]
  pub fn map_error<E2, F>(self, map: F) -> Exit<A, E2>
  where
    F: Fn(E) -> E2 + Copy,
  {
    match self {
      Exit::Success(value) => Exit::Success(value),
      Exit::Failure(cause) => Exit::Failure(cause.map_fail(map)),
    }
  }

  /// `Exit` as [`Either`] — `Success` → `Ok`, `Failure` → `Err`.
  #[inline]
  pub fn to_either(self) -> Either<A, Cause<E>>
  where
    A: 'static,
    E: 'static,
  {
    self.into_result()
  }

  /// Build an [`Exit`] from an [`Either`] over success value and failure [`Cause`].
  #[inline]
  pub fn from_either(e: Either<A, Cause<E>>) -> Self {
    match e {
      Ok(value) => Exit::Success(value),
      Err(cause) => Exit::Failure(cause),
    }
  }

  /// Converts to `Result` — success value or failure [`Cause`].
  #[inline]
  pub fn into_result(self) -> Result<A, Cause<E>>
  where
    A: 'static,
    E: 'static,
  {
    Matcher::new()
      .when(
        Box::new(|e: &Exit<A, E>| matches!(e, Exit::Success(_))),
        |e| match e {
          Exit::Success(value) => Ok(value),
          _ => unreachable!(),
        },
      )
      .when(
        Box::new(|e: &Exit<A, E>| matches!(e, Exit::Failure(_))),
        |e| match e {
          Exit::Failure(cause) => Err(cause),
          _ => unreachable!(),
        },
      )
      .run_exhaustive(self)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn succeed_with_value_returns_success_variant() {
      assert_eq!(Exit::<u8, &str>::succeed(7), Exit::Success(7));
    }

    #[test]
    fn fail_with_error_returns_failure_with_fail_cause() {
      assert_eq!(
        Exit::<u8, &str>::fail("boom"),
        Exit::Failure(Cause::fail("boom"))
      );
    }

    #[test]
    fn die_with_message_returns_failure_with_die_cause() {
      assert_eq!(
        Exit::<u8, &str>::die("defect"),
        Exit::Failure(Cause::<&str>::die("defect"))
      );
    }

    #[test]
    fn interrupt_with_fiber_id_returns_failure_with_interrupt_cause() {
      let fiber_id = FiberId::fresh();
      assert_eq!(
        Exit::<u8, &str>::interrupt(fiber_id),
        Exit::Failure(Cause::interrupt(fiber_id))
      );
    }
  }

  mod map {
    use super::*;

    #[test]
    fn map_with_success_transforms_success_value() {
      let mapped = Exit::<u8, &str>::succeed(2).map(|n| n * 3);
      assert_eq!(mapped, Exit::Success(6));
    }

    #[test]
    fn map_with_failure_preserves_original_cause() {
      let source = Exit::<u8, &str>::fail("boom");
      let mapped = source.map(|n| n * 3);
      assert_eq!(mapped, Exit::Failure(Cause::fail("boom")));
    }
  }

  mod map_error {
    use super::*;

    #[test]
    fn map_error_with_success_preserves_success_value() {
      let mapped = Exit::<u8, u8>::succeed(9).map_error(|n| n.to_string());
      assert_eq!(mapped, Exit::Success(9));
    }

    #[test]
    fn map_error_with_fail_cause_transforms_typed_error() {
      let mapped = Exit::<u8, u8>::fail(4).map_error(|n| n.to_string());
      assert_eq!(mapped, Exit::Failure(Cause::fail(String::from("4"))));
    }

    #[test]
    fn map_error_with_die_cause_preserves_defect_message() {
      let mapped = Exit::<u8, u8>::die("fatal").map_error(|n| n.to_string());
      assert_eq!(mapped, Exit::Failure(Cause::Die(String::from("fatal"))));
    }

    #[test]
    fn map_error_with_interrupt_cause_preserves_fiber_id() {
      let fiber_id = FiberId::fresh();
      let mapped = Exit::<u8, u8>::interrupt(fiber_id).map_error(|n| n.to_string());
      assert_eq!(mapped, Exit::Failure(Cause::Interrupt(fiber_id)));
    }
  }

  mod into_result {
    use super::*;

    #[rstest]
    #[case::success(Exit::<u8, &str>::succeed(3), Ok(3))]
    #[case::failed(Exit::<u8, &str>::fail("boom"), Err(Cause::Fail("boom")))]
    fn into_result_with_success_or_fail_converts_to_result(
      #[case] exit: Exit<u8, &'static str>,
      #[case] expected: Result<u8, Cause<&'static str>>,
    ) {
      assert_eq!(exit.into_result(), expected);
    }

    #[test]
    fn into_result_with_interrupt_preserves_interrupt_cause_information() {
      let interrupted = Exit::<u8, &str>::interrupt(FiberId::fresh()).into_result();
      assert!(matches!(interrupted, Err(Cause::Interrupt(_))));
    }
  }

  mod effect_data {
    use super::*;

    #[test]
    fn exit_success_ne_failure_same_value() {
      let ok = Exit::<i32, i32>::succeed(1);
      let bad = Exit::<i32, i32>::fail(1);
      assert_ne!(ok, bad);
    }
  }

  mod to_either_from_either {
    use super::*;
    use crate::Either;

    #[rstest]
    #[case::success(Exit::<i32, &'static str>::succeed(42))]
    #[case::fail(Exit::<i32, &'static str>::fail("err"))]
    #[case::die(Exit::<i32, &'static str>::die("defect"))]
    fn to_either_then_from_either_roundtrips_exit(#[case] original: Exit<i32, &'static str>) {
      let roundtrip = Exit::from_either(original.clone().to_either());
      assert_eq!(roundtrip, original);
    }

    #[test]
    fn to_either_then_from_either_roundtrips_interrupt() {
      let fiber_id = FiberId::fresh();
      let original = Exit::<i32, &str>::interrupt(fiber_id);
      let roundtrip = Exit::from_either(original.clone().to_either());
      assert_eq!(roundtrip, original);
    }

    #[rstest]
    #[case::ok_ok(Ok(1_i32), Exit::succeed(1))]
    #[case::ok_err(Err(Cause::fail("e")), Exit::Failure(Cause::fail("e")))]
    fn from_either_builds_expected_exit(
      #[case] e: Either<i32, Cause<&'static str>>,
      #[case] expected: Exit<i32, &'static str>,
    ) {
      assert_eq!(Exit::from_either(e), expected);
    }
  }
}
