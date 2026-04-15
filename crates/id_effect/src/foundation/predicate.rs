//! Composable predicates — mirrors Effect.ts `Predicate` namespace.
//!
//! `Predicate<A>` is a heap-allocated, `Send + Sync` boolean function over `&A`.
//! All combinators return new `Predicate<A>` values, enabling runtime composition.

// ── Predicate<A> ─────────────────────────────────────────────────────────────

/// A runtime-composable boolean function — mirrors Effect.ts `Predicate<A>`.
///
/// Heap-allocated and `Send + Sync` so predicates can be stored and shared
/// across threads without lifetime constraints.
pub type Predicate<A> = Box<dyn Fn(&A) -> bool + Send + Sync>;

// ── predicate module ─────────────────────────────────────────────────────────

/// Constructors and combinators for `Predicate<A>`.
#[allow(clippy::module_inception)]
pub mod predicate {
  use super::Predicate;

  // ── Boolean combinators ───────────────────────────────────────────────────

  /// `p AND q` — true when both predicates hold.
  pub fn and<A: 'static>(p: Predicate<A>, q: Predicate<A>) -> Predicate<A> {
    Box::new(move |a| p(a) && q(a))
  }

  /// `p OR q` — true when at least one predicate holds.
  pub fn or<A: 'static>(p: Predicate<A>, q: Predicate<A>) -> Predicate<A> {
    Box::new(move |a| p(a) || q(a))
  }

  /// `NOT p` — true when the predicate does not hold.
  pub fn not<A: 'static>(p: Predicate<A>) -> Predicate<A> {
    Box::new(move |a| !p(a))
  }

  /// `p XOR q` — true when exactly one predicate holds.
  pub fn xor<A: 'static>(p: Predicate<A>, q: Predicate<A>) -> Predicate<A> {
    Box::new(move |a| p(a) ^ q(a))
  }

  /// `p IMPLIES q` — equivalent to `NOT p OR q`.
  pub fn implies<A: 'static>(p: Predicate<A>, q: Predicate<A>) -> Predicate<A> {
    Box::new(move |a| !p(a) || q(a))
  }

  /// `p XNOR q` (equivalence) — true when both hold or neither holds.
  pub fn eqv<A: 'static>(p: Predicate<A>, q: Predicate<A>) -> Predicate<A> {
    Box::new(move |a| p(a) == q(a))
  }

  // ── Contramap ────────────────────────────────────────────────────────────

  /// Contramap: apply `f` to the input before testing with `p`.
  ///
  /// Mirrors Effect.ts `Predicate.contramap`.
  pub fn contramap<A: 'static, B: 'static>(
    p: Predicate<A>,
    f: impl Fn(&B) -> A + Send + Sync + 'static,
  ) -> Predicate<B> {
    Box::new(move |b| p(&f(b)))
  }

  // ── Product ──────────────────────────────────────────────────────────────

  /// Test a tuple `(A, B)` by applying `p` to the first element and `q` to the second.
  pub fn product<A: 'static, B: 'static>(p: Predicate<A>, q: Predicate<B>) -> Predicate<(A, B)> {
    Box::new(move |(a, b)| p(a) && q(b))
  }

  // ── All ──────────────────────────────────────────────────────────────────

  /// Test a `Vec<A>` element-wise: true when every element satisfies its corresponding predicate.
  ///
  /// If `predicates` is shorter than `values`, the remaining values are ignored.
  pub fn all<A: 'static>(predicates: Vec<Predicate<A>>) -> Predicate<Vec<A>> {
    Box::new(move |values| predicates.iter().zip(values.iter()).all(|(p, v)| p(v)))
  }

  // ── Built-in refinements ──────────────────────────────────────────────────

  /// True when `Option<A>` is `Some`.
  pub fn is_some<A: 'static>() -> Predicate<Option<A>> {
    Box::new(|o| o.is_some())
  }

  /// True when `Option<A>` is `None`.
  pub fn is_none<A: 'static>() -> Predicate<Option<A>> {
    Box::new(|o| o.is_none())
  }

  /// True when `Result<A, E>` is `Ok`.
  pub fn is_ok<A: 'static, E: 'static>() -> Predicate<Result<A, E>> {
    Box::new(|r| r.is_ok())
  }

  /// True when `Result<A, E>` is `Err`.
  pub fn is_err<A: 'static, E: 'static>() -> Predicate<Result<A, E>> {
    Box::new(|r| r.is_err())
  }

  /// True when `String` is empty.
  pub fn is_empty() -> Predicate<String> {
    Box::new(|s: &String| s.is_empty())
  }

  /// True when `String` is non-empty.
  pub fn is_non_empty() -> Predicate<String> {
    Box::new(|s: &String| !s.is_empty())
  }

  /// True when `&str` is empty.
  pub fn str_is_empty() -> Predicate<str> {
    Box::new(|s: &str| s.is_empty())
  }

  /// True when a numeric value is zero.
  pub fn is_zero_i64() -> Predicate<i64> {
    Box::new(|n| *n == 0)
  }

  /// True when a numeric value is positive (> 0).
  pub fn is_positive_i64() -> Predicate<i64> {
    Box::new(|n| *n > 0)
  }

  /// True when a numeric value is negative (< 0).
  pub fn is_negative_i64() -> Predicate<i64> {
    Box::new(|n| *n < 0)
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::predicate;
  use rstest::rstest;

  // Helper: a predicate that tests whether an i64 is > 0
  fn positive() -> super::Predicate<i64> {
    Box::new(|n: &i64| *n > 0)
  }

  // Helper: a predicate that tests whether an i64 is even
  fn even() -> super::Predicate<i64> {
    Box::new(|n: &i64| *n % 2 == 0)
  }

  // ── and ──────────────────────────────────────────────────────────────────

  mod and {
    use super::*;

    #[rstest]
    #[case::both_true(2_i64, true)] // positive AND even
    #[case::first_false(-2_i64, false)] // not positive, even → false
    #[case::second_false(3_i64, false)] // positive, not even → false
    #[case::both_false(-3_i64, false)] // not positive, not even
    fn truth_table(#[case] input: i64, #[case] expected: bool) {
      let p = predicate::and(positive(), even());
      assert_eq!(p(&input), expected);
    }
  }

  // ── or ───────────────────────────────────────────────────────────────────

  mod or {
    use super::*;

    #[rstest]
    #[case::both_true(2_i64, true)]
    #[case::first_only(3_i64, true)]
    #[case::second_only(-2_i64, true)]
    #[case::neither(-3_i64, false)]
    fn truth_table(#[case] input: i64, #[case] expected: bool) {
      let p = predicate::or(positive(), even());
      assert_eq!(p(&input), expected);
    }
  }

  // ── not ──────────────────────────────────────────────────────────────────

  mod not {
    use super::*;

    #[test]
    fn not_positive_is_true_for_zero() {
      let p = predicate::not(positive());
      assert!(p(&0_i64));
    }

    #[test]
    fn not_positive_is_true_for_negative() {
      let p = predicate::not(positive());
      assert!(p(&-5_i64));
    }

    #[test]
    fn not_positive_is_false_for_positive() {
      let p = predicate::not(positive());
      assert!(!p(&1_i64));
    }
  }

  // ── xor ──────────────────────────────────────────────────────────────────

  mod xor {
    use super::*;

    #[rstest]
    #[case::both_true(2_i64, false)]
    #[case::first_only(3_i64, true)]
    #[case::second_only(-2_i64, true)]
    #[case::neither(-3_i64, false)]
    fn truth_table(#[case] input: i64, #[case] expected: bool) {
      let p = predicate::xor(positive(), even());
      assert_eq!(p(&input), expected);
    }
  }

  // ── implies ──────────────────────────────────────────────────────────────

  mod implies {
    use super::*;

    #[test]
    fn true_implies_true_is_true() {
      let p = predicate::implies(positive(), even());
      assert!(p(&2_i64)); // positive → even: T → T = T
    }

    #[test]
    fn true_implies_false_is_false() {
      let p = predicate::implies(positive(), even());
      assert!(!p(&3_i64)); // positive → even: T → F = F
    }

    #[test]
    fn false_implies_anything_is_true() {
      let p = predicate::implies(positive(), even());
      assert!(p(&-3_i64)); // positive → even: F → F = T
      assert!(p(&-2_i64)); // positive → even: F → T = T
    }
  }

  // ── eqv ──────────────────────────────────────────────────────────────────

  mod eqv {
    use super::*;

    #[rstest]
    #[case::both_true(2_i64, true)]
    #[case::both_false(-3_i64, true)]
    #[case::first_only(3_i64, false)]
    #[case::second_only(-2_i64, false)]
    fn truth_table(#[case] input: i64, #[case] expected: bool) {
      let p = predicate::eqv(positive(), even());
      assert_eq!(p(&input), expected);
    }
  }

  // ── contramap ────────────────────────────────────────────────────────────

  mod contramap {
    use super::*;

    #[test]
    fn test_string_by_length_positive() {
      // Map string → its length, then test positive
      let p = predicate::contramap(positive(), |s: &String| s.len() as i64);
      assert!(p(&"hello".to_string()));
    }

    #[test]
    fn test_empty_string_length_not_positive() {
      let p = predicate::contramap(positive(), |s: &String| s.len() as i64);
      assert!(!p(&"".to_string()));
    }
  }

  // ── product ──────────────────────────────────────────────────────────────

  mod product {
    use super::*;

    #[test]
    fn both_hold_returns_true() {
      let p = predicate::product(positive(), even());
      assert!(p(&(4_i64, 6_i64)));
    }

    #[test]
    fn first_fails_returns_false() {
      let p = predicate::product(positive(), even());
      assert!(!p(&(-1_i64, 4_i64)));
    }

    #[test]
    fn second_fails_returns_false() {
      let p = predicate::product(positive(), even());
      assert!(!p(&(3_i64, 3_i64)));
    }

    #[test]
    fn both_fail_returns_false() {
      let p = predicate::product(positive(), even());
      assert!(!p(&(-1_i64, 3_i64)));
    }
  }

  // ── all ──────────────────────────────────────────────────────────────────

  mod all {
    use super::*;

    #[test]
    fn all_positive_values_returns_true() {
      let ps = vec![positive(), positive(), positive()];
      let p = predicate::all(ps);
      assert!(p(&vec![1_i64, 2_i64, 3_i64]));
    }

    #[test]
    fn one_failing_value_returns_false() {
      let ps = vec![positive(), positive(), positive()];
      let p = predicate::all(ps);
      assert!(!p(&vec![1_i64, -1_i64, 3_i64]));
    }

    #[test]
    fn empty_predicates_returns_true_for_any_vec() {
      let ps: Vec<super::super::Predicate<i64>> = vec![];
      let p = predicate::all(ps);
      assert!(p(&vec![1_i64, 2_i64]));
    }
  }

  // ── built-in refinements ─────────────────────────────────────────────────

  mod builtins {
    use super::*;

    #[test]
    fn is_some_true_for_some() {
      assert!(predicate::is_some::<i32>()(&Some(1)));
    }

    #[test]
    fn is_some_false_for_none() {
      assert!(!predicate::is_some::<i32>()(&None));
    }

    #[test]
    fn is_none_true_for_none() {
      assert!(predicate::is_none::<i32>()(&None));
    }

    #[test]
    fn is_none_false_for_some() {
      assert!(!predicate::is_none::<i32>()(&Some(5)));
    }

    #[test]
    fn is_ok_true_for_ok() {
      let p = predicate::is_ok::<i32, &str>();
      assert!(p(&Ok(1)));
    }

    #[test]
    fn is_ok_false_for_err() {
      let p = predicate::is_ok::<i32, &str>();
      assert!(!p(&Err("oops")));
    }

    #[test]
    fn is_err_true_for_err() {
      let p = predicate::is_err::<i32, &str>();
      assert!(p(&Err("bad")));
    }

    #[test]
    fn is_err_false_for_ok() {
      let p = predicate::is_err::<i32, &str>();
      assert!(!p(&Ok(42)));
    }

    #[test]
    fn is_empty_true_for_empty_string() {
      assert!(predicate::is_empty()(&"".to_string()));
    }

    #[test]
    fn is_empty_false_for_non_empty_string() {
      assert!(!predicate::is_empty()(&"hi".to_string()));
    }

    #[test]
    fn is_non_empty_true_for_non_empty() {
      assert!(predicate::is_non_empty()(&"x".to_string()));
    }

    #[test]
    fn is_non_empty_false_for_empty() {
      assert!(!predicate::is_non_empty()(&"".to_string()));
    }

    #[rstest]
    #[case::zero(0_i64, true)]
    #[case::positive(1_i64, false)]
    #[case::negative(-1_i64, false)]
    fn is_zero_i64(#[case] input: i64, #[case] expected: bool) {
      assert_eq!(predicate::is_zero_i64()(&input), expected);
    }

    #[rstest]
    #[case::positive(5_i64, true)]
    #[case::zero(0_i64, false)]
    #[case::negative(-1_i64, false)]
    fn is_positive_i64(#[case] input: i64, #[case] expected: bool) {
      assert_eq!(predicate::is_positive_i64()(&input), expected);
    }

    #[rstest]
    #[case::negative(-1_i64, true)]
    #[case::zero(0_i64, false)]
    #[case::positive(1_i64, false)]
    fn is_negative_i64(#[case] input: i64, #[case] expected: bool) {
      assert_eq!(predicate::is_negative_i64()(&input), expected);
    }
  }
}
