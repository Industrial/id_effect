//! Free-function combinators over `Option<T>` — mirrors the Effect.ts `Option` namespace.
//!
//! Rust's `std::option::Option<T>` is already in the prelude; this module provides
//! the Effect.ts-named companion functions (`map`, `flat_map`, `get_or_else`, etc.)
//! so code reading like Effect.ts can be written in Rust without chaining method calls.
//!
//! Import the submodule:
//! ```rust,ignore
//! use id_effect::option_::option;
//! let v = option::get_or_else(None, || 42_i32);
//! ```

/// Constructors and combinators for `Option<T>` — mirrors Effect.ts `Option` namespace.
pub mod option {
  // ── Constructors ──────────────────────────────────────────────────────────

  /// Wrap `a` in `Some` (`Option.some`).
  pub fn some<A>(a: A) -> Option<A> {
    Some(a)
  }

  /// The absent value (`Option.none`).
  pub fn none<A>() -> Option<A> {
    None
  }

  // ── Conversions ───────────────────────────────────────────────────────────

  /// Convert `Result<A, E>` to `Option<A>`, discarding the error.
  pub fn from_result<A, E>(r: Result<A, E>) -> Option<A> {
    r.ok()
  }

  /// Convert `Option<A>` to `Result<A, E>`, using `error()` when absent.
  pub fn to_result<A, E>(o: Option<A>, error: impl FnOnce() -> E) -> Result<A, E> {
    o.ok_or_else(error)
  }

  // ── Transformations ───────────────────────────────────────────────────────

  /// Apply `f` to the inner value if `Some`, propagate `None` unchanged.
  pub fn map<A, B>(o: Option<A>, f: impl FnOnce(A) -> B) -> Option<B> {
    o.map(f)
  }

  /// Flat-map: apply `f` which itself may return `None`.
  pub fn flat_map<A, B>(o: Option<A>, f: impl FnOnce(A) -> Option<B>) -> Option<B> {
    o.and_then(f)
  }

  /// Return the inner value, or compute a default with `default()`.
  pub fn get_or_else<A>(o: Option<A>, default: impl FnOnce() -> A) -> A {
    o.unwrap_or_else(default)
  }

  /// Return `o` if it is `Some`, otherwise try `alt()`.
  pub fn or_else<A>(o: Option<A>, alt: impl FnOnce() -> Option<A>) -> Option<A> {
    o.or_else(alt)
  }

  /// Keep the value only if `p` returns `true`.
  pub fn filter<A>(o: Option<A>, p: impl FnOnce(&A) -> bool) -> Option<A> {
    o.filter(p)
  }

  // ── Combining ────────────────────────────────────────────────────────────

  /// Combine two `Option`s into a tuple — `None` if either is absent.
  pub fn zip<A, B>(a: Option<A>, b: Option<B>) -> Option<(A, B)> {
    a.zip(b)
  }

  /// Combine two `Option`s with a function — `None` if either is absent.
  pub fn zip_with<A, B, C>(a: Option<A>, b: Option<B>, f: impl FnOnce(A, B) -> C) -> Option<C> {
    a.zip(b).map(|(a, b)| f(a, b))
  }

  // ── Side effects ─────────────────────────────────────────────────────────

  /// Call `f` with a reference to the inner value if `Some`; return the original `Option`.
  pub fn tap<A>(o: Option<A>, f: impl FnOnce(&A)) -> Option<A> {
    if let Some(ref v) = o {
      f(v);
    }
    o
  }

  // ── Lifting ──────────────────────────────────────────────────────────────

  /// Return `Some(a)` if `p(&a)` is true, `None` otherwise.
  ///
  /// Mirrors Effect.ts `Option.liftPredicate`.
  pub fn lift_predicate<A>(a: A, p: impl Fn(&A) -> bool) -> Option<A> {
    if p(&a) { Some(a) } else { None }
  }

  /// Same as [`lift_predicate`] — mirrors Effect.ts `Option.fromPredicate`.
  #[inline]
  pub fn from_predicate<A>(a: A, p: impl Fn(&A) -> bool) -> Option<A> {
    lift_predicate(a, p)
  }

  // ── Inspection ───────────────────────────────────────────────────────────

  /// True when the option is `Some`.
  pub fn is_some<A>(o: &Option<A>) -> bool {
    o.is_some()
  }

  /// True when the option is `None`.
  pub fn is_none<A>(o: &Option<A>) -> bool {
    o.is_none()
  }

  /// Flatten a nested `Option<Option<A>>` into `Option<A>`.
  pub fn flatten<A>(o: Option<Option<A>>) -> Option<A> {
    o.flatten()
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::option;
  use rstest::rstest;

  // ── constructors ─────────────────────────────────────────────────────────

  mod constructors {
    use super::*;

    #[test]
    fn some_wraps_value() {
      assert_eq!(option::some(42_i32), Some(42));
    }

    #[test]
    fn none_returns_no_value() {
      assert_eq!(option::none::<i32>(), None);
    }
  }

  // ── from_result / to_result ───────────────────────────────────────────────

  mod conversions {
    use super::*;

    #[test]
    fn from_result_ok_returns_some() {
      assert_eq!(option::from_result(Ok::<i32, &str>(5)), Some(5));
    }

    #[test]
    fn from_result_err_returns_none() {
      assert_eq!(option::from_result(Err::<i32, &str>("fail")), None);
    }

    #[test]
    fn to_result_some_returns_ok() {
      assert_eq!(option::to_result(Some(1_i32), || "missing"), Ok(1));
    }

    #[test]
    fn to_result_none_returns_err() {
      assert_eq!(option::to_result(None::<i32>, || "missing"), Err("missing"));
    }
  }

  // ── map ──────────────────────────────────────────────────────────────────

  mod map {
    use super::*;

    #[test]
    fn some_maps_value() {
      assert_eq!(option::map(Some(3_i32), |n| n * 2), Some(6));
    }

    #[test]
    fn none_stays_none() {
      assert_eq!(option::map(None::<i32>, |n| n * 2), None);
    }
  }

  // ── flat_map ─────────────────────────────────────────────────────────────

  mod flat_map {
    use super::*;

    #[test]
    fn some_to_some() {
      assert_eq!(option::flat_map(Some(4_i32), |n| Some(n + 1)), Some(5));
    }

    #[test]
    fn some_to_none() {
      assert_eq!(option::flat_map(Some(0_i32), |_| None::<i32>), None);
    }

    #[test]
    fn none_stays_none() {
      assert_eq!(option::flat_map(None::<i32>, Some), None);
    }
  }

  // ── get_or_else ──────────────────────────────────────────────────────────

  mod get_or_else {
    use super::*;

    #[test]
    fn some_returns_inner() {
      assert_eq!(option::get_or_else(Some(7_i32), || 99), 7);
    }

    #[test]
    fn none_returns_default() {
      assert_eq!(option::get_or_else(None::<i32>, || 99), 99);
    }
  }

  // ── or_else ───────────────────────────────────────────────────────────────

  mod or_else {
    use super::*;

    #[test]
    fn some_ignores_alternative() {
      assert_eq!(option::or_else(Some(1_i32), || Some(2)), Some(1));
    }

    #[test]
    fn none_uses_alternative_when_some() {
      assert_eq!(option::or_else(None::<i32>, || Some(2)), Some(2));
    }

    #[test]
    fn none_uses_alternative_when_also_none() {
      assert_eq!(option::or_else(None::<i32>, || None), None);
    }
  }

  // ── filter ────────────────────────────────────────────────────────────────

  mod filter {
    use super::*;

    #[test]
    fn some_passing_predicate_stays_some() {
      assert_eq!(option::filter(Some(4_i32), |n| *n % 2 == 0), Some(4));
    }

    #[test]
    fn some_failing_predicate_becomes_none() {
      assert_eq!(option::filter(Some(3_i32), |n| *n % 2 == 0), None);
    }

    #[test]
    fn none_stays_none() {
      assert_eq!(option::filter(None::<i32>, |_| true), None);
    }
  }

  // ── zip / zip_with ────────────────────────────────────────────────────────

  mod zip {
    use super::*;

    #[rstest]
    #[case::both_some(Some(1_i32), Some("a"), Some((1_i32, "a")))]
    #[case::first_none(None, Some("a"), None)]
    #[case::second_none(Some(1_i32), None, None)]
    #[case::both_none(None, None, None)]
    fn zip_cases(
      #[case] a: Option<i32>,
      #[case] b: Option<&str>,
      #[case] expected: Option<(i32, &str)>,
    ) {
      assert_eq!(option::zip(a, b), expected);
    }

    #[test]
    fn zip_with_some_some_applies_function() {
      assert_eq!(
        option::zip_with(Some(3_i32), Some(4_i32), |a, b| a + b),
        Some(7)
      );
    }

    #[test]
    fn zip_with_some_none_returns_none() {
      assert_eq!(
        option::zip_with(Some(1_i32), None::<i32>, |a, b| a + b),
        None
      );
    }
  }

  // ── tap ───────────────────────────────────────────────────────────────────

  mod tap {
    use super::*;

    #[test]
    fn tap_on_some_calls_f_and_returns_some() {
      let mut called_with = None;
      let result = option::tap(Some(42_i32), |v| called_with = Some(*v));
      assert_eq!(result, Some(42));
      assert_eq!(called_with, Some(42));
    }

    #[test]
    fn tap_on_none_does_not_call_f() {
      let mut called = false;
      let result = option::tap(None::<i32>, |_| called = true);
      assert_eq!(result, None);
      assert!(!called);
    }
  }

  // ── lift_predicate ────────────────────────────────────────────────────────

  mod lift_predicate {
    use super::*;

    #[test]
    fn passing_predicate_returns_some() {
      assert_eq!(option::lift_predicate(4_i32, |n| *n > 0), Some(4));
    }

    #[test]
    fn failing_predicate_returns_none() {
      assert_eq!(option::lift_predicate(-1_i32, |n| *n > 0), None);
    }

    #[rstest]
    #[case::positive(5_i32, true)]
    #[case::zero(0_i32, false)]
    #[case::negative(-1_i32, false)]
    fn parametrised(#[case] v: i32, #[case] should_be_some: bool) {
      let result = option::lift_predicate(v, |n| *n > 0);
      assert_eq!(result.is_some(), should_be_some);
    }
  }

  // ── is_some / is_none ────────────────────────────────────────────────────

  mod inspection {
    use super::*;

    #[test]
    fn is_some_true_for_some() {
      assert!(option::is_some(&Some(1_i32)));
    }

    #[test]
    fn is_some_false_for_none() {
      assert!(!option::is_some(&None::<i32>));
    }

    #[test]
    fn is_none_true_for_none() {
      assert!(option::is_none(&None::<i32>));
    }

    #[test]
    fn is_none_false_for_some() {
      assert!(!option::is_none(&Some(1_i32)));
    }
  }

  // ── flatten ───────────────────────────────────────────────────────────────

  mod flatten {
    use super::*;

    #[test]
    fn some_some_flattens_to_some() {
      assert_eq!(option::flatten(Some(Some(7_i32))), Some(7));
    }

    #[test]
    fn some_none_flattens_to_none() {
      assert_eq!(option::flatten(Some(None::<i32>)), None);
    }

    #[test]
    fn outer_none_flattens_to_none() {
      assert_eq!(option::flatten(None::<Option<i32>>), None);
    }
  }
}
