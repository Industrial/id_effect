//! [`Or`] — binary sum type for widened error channels.
//!
//! Rust has no native open union type; [`Or`] is the explicit equivalent used by
//! combinators that widen error channels.

use crate::foundation::either::Either;

/// Binary sum type (`L | R`) for error channels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Or<L, R> {
  /// First / typically "upstream" error arm in widened channels.
  Left(L),
  /// Second / typically "mapper" error arm in widened channels.
  Right(R),
}

impl<L, R> Or<L, R> {
  /// [`Or::Right`] → `Ok`, [`Or::Left`] → `Err` (left is the `Either` left / error side).
  #[inline]
  pub fn to_either(self) -> Either<R, L> {
    match self {
      Or::Left(l) => Err(l),
      Or::Right(r) => Ok(r),
    }
  }

  /// Inverse of [`Or::to_either`]: `Ok` → [`Or::Right`], `Err` → [`Or::Left`].
  #[inline]
  pub fn from_either(e: Either<R, L>) -> Self {
    match e {
      Ok(r) => Or::Right(r),
      Err(l) => Or::Left(l),
    }
  }
}

impl<L, R> From<L> for Or<L, R> {
  #[inline]
  fn from(value: L) -> Self {
    Self::Left(value)
  }
}

impl<E> Or<E, E> {
  /// Collapse `Or<E, E>` to `E` — both arms carry the same type.
  ///
  /// ```text
  /// unify: Or[E, E] → E
  /// ```
  #[inline]
  pub fn unify(self) -> E {
    match self {
      Or::Left(e) | Or::Right(e) => e,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod from_impl {
    use super::*;

    #[test]
    fn from_with_left_value_wraps_value_in_left_variant() {
      let out: Or<i32, &'static str> = 7.into();
      assert_eq!(out, Or::Left(7));
    }
  }

  mod variant_detection {
    use super::*;

    #[rstest]
    #[case::left(Or::<i32, &'static str>::Left(1), true)]
    #[case::right(Or::<i32, &'static str>::Right("x"), false)]
    fn left_detection_with_variant_matches_expected_variant_flag(
      #[case] value: Or<i32, &'static str>,
      #[case] expected_is_left: bool,
    ) {
      let is_left = matches!(value, Or::Left(_));
      assert_eq!(is_left, expected_is_left);
    }
  }

  mod to_either_from_either {
    use super::*;

    #[rstest]
    #[case::left(Or::<i32, &'static str>::Left(-1))]
    #[case::right(Or::<i32, &'static str>::Right("ok"))]
    fn to_either_then_from_either_roundtrips_or(#[case] original: Or<i32, &'static str>) {
      let roundtrip = Or::from_either(original.clone().to_either());
      assert_eq!(roundtrip, original);
    }

    #[rstest]
    #[case::ok(Ok("r"), Or::Right("r"))]
    #[case::err(Err(9_i32), Or::Left(9))]
    fn from_either_builds_expected_or(
      #[case] e: Result<&'static str, i32>,
      #[case] expected: Or<i32, &'static str>,
    ) {
      assert_eq!(Or::from_either(e), expected);
    }
  }

  mod unify {
    use super::*;

    #[test]
    fn unify_left_variant_returns_inner_value() {
      let or: Or<i32, i32> = Or::Left(42);
      assert_eq!(or.unify(), 42);
    }

    #[test]
    fn unify_right_variant_returns_inner_value() {
      let or: Or<i32, i32> = Or::Right(99);
      assert_eq!(or.unify(), 99);
    }

    #[test]
    fn unify_is_idempotent_for_both_arms() {
      let left: Or<&str, &str> = Or::Left("x");
      let right: Or<&str, &str> = Or::Right("x");
      assert_eq!(left.unify(), right.unify());
    }
  }
}
