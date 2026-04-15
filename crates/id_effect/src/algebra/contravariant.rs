//! **Contravariant** — a functor with reversed variance.
//!
//! A contravariant functor is the dual of a covariant functor. Where a regular
//! functor maps `A → B` to `F<A> → F<B>`, a contravariant functor maps
//! `A → B` to `F<B> → F<A>` (note the reversal).
//!
//! ## Definition
//!
//! ```text
//! CONTRAVARIANT[F] ::= (F<_>, contramap: (A → B) → F<B> → F<A>)
//! ```
//!
//! ## Laws
//!
//! - **Identity**: `contramap(id)(fa) = fa`
//! - **Composition**: `contramap(f ∘ g) = contramap(g) ∘ contramap(f)` (note: reversed!)
//!
//! ## Examples in this system
//!
//! - `Fn(A) -> R` — contravariant in `A` (can precompose with `B → A`)
//! - `Predicate<A>` — contravariant in `A`
//! - `Comparator<A>` — contravariant in `A`
//! - `Equivalence<A>` — contravariant in `A`
//!
//! ## Intuition
//!
//! Contravariance appears when a type is in "input position". If you have
//! a `Predicate<Cat>` and want a `Predicate<Animal>`, you can contramap
//! with `Animal → Cat` (e.g., a function that extracts the cat-ness).
//!
//! ## Relationship to Stratum 0
//!
//! - Uses: [`compose`](super::super::foundation::function::compose) — but arguments reversed

/// A functor with reversed variance (contravariant functor).
///
/// # Laws
///
/// ```text
/// contramap(|x| x)(fa) = fa                         // Identity
/// contramap(|x| f(g(x))) = contramap(g).contramap(f)  // Composition (reversed)
/// ```
///
/// Note the composition law is reversed from regular functors!
pub trait Contravariant {
  /// The type this contravariant functor is parameterized over.
  type Inner;

  /// The result of contramapping to type `B`.
  type Output<B>;

  /// Transform by precomposing with a function.
  ///
  /// Given `F<A>` and `f: B → A`, produce `F<B>`.
  fn contramap<B>(self, f: impl Fn(B) -> Self::Inner) -> Self::Output<B>;
}

/// Contramap over a contravariant functor (free function).
#[inline]
pub fn contramap<F: Contravariant, B>(fa: F, f: impl Fn(B) -> F::Inner) -> F::Output<B> {
  fa.contramap(f)
}

// ── Predicate ────────────────────────────────────────────────────────────────

// Note: A general Contravariant trait impl for closures is tricky in Rust
// due to ownership and lifetime constraints. We provide concrete types instead.

/// A reference-based predicate (more practical for contravariance).
#[derive(Clone)]
pub struct PredicateRef<A: ?Sized> {
  run: std::sync::Arc<dyn Fn(&A) -> bool + Send + Sync>,
}

impl<A: ?Sized> PredicateRef<A> {
  /// Create a new predicate from a function.
  pub fn new(f: impl Fn(&A) -> bool + Send + Sync + 'static) -> Self {
    Self {
      run: std::sync::Arc::new(f),
    }
  }

  /// Test a value against this predicate.
  #[inline]
  pub fn test(&self, a: &A) -> bool {
    (self.run)(a)
  }

  /// Negate this predicate.
  pub fn not(self) -> Self
  where
    A: 'static,
  {
    let run = self.run.clone();
    Self::new(move |a| !(run)(a))
  }

  /// Combine with another predicate using AND.
  pub fn and(self, other: Self) -> Self
  where
    A: 'static,
  {
    let run1 = self.run.clone();
    let run2 = other.run.clone();
    Self::new(move |a| (run1)(a) && (run2)(a))
  }

  /// Combine with another predicate using OR.
  pub fn or(self, other: Self) -> Self
  where
    A: 'static,
  {
    let run1 = self.run.clone();
    let run2 = other.run.clone();
    Self::new(move |a| (run1)(a) || (run2)(a))
  }

  /// Contramap: transform the input type.
  ///
  /// Given `Predicate<A>` and `f: &B → &A`, get `Predicate<B>`.
  pub fn contramap_ref<B: ?Sized + 'static>(
    self,
    f: impl Fn(&B) -> &A + Send + Sync + 'static,
  ) -> PredicateRef<B>
  where
    A: 'static,
  {
    let run = self.run.clone();
    PredicateRef::new(move |b| (run)(f(b)))
  }
}

// ── Equivalence ──────────────────────────────────────────────────────────────

/// An equivalence relation: determines if two values are equivalent.
#[derive(Clone)]
pub struct Equivalence<A> {
  /// The equivalence check function.
  pub eq: std::sync::Arc<dyn Fn(&A, &A) -> bool + Send + Sync>,
}

impl<A> Equivalence<A> {
  /// Create a new equivalence from a function.
  pub fn new(f: impl Fn(&A, &A) -> bool + Send + Sync + 'static) -> Self {
    Self {
      eq: std::sync::Arc::new(f),
    }
  }

  /// Check if two values are equivalent.
  #[inline]
  pub fn equivalent(&self, a: &A, b: &A) -> bool {
    (self.eq)(a, b)
  }

  /// Contramap: transform the input type.
  pub fn contramap_with<B: 'static>(
    self,
    f: impl Fn(&B) -> A + Send + Sync + 'static,
  ) -> Equivalence<B>
  where
    A: 'static,
  {
    let eq = self.eq.clone();
    Equivalence::new(move |b1, b2| (eq)(&f(b1), &f(b2)))
  }
}

/// Create an equivalence from `PartialEq`.
pub fn from_eq<A: PartialEq + 'static>() -> Equivalence<A> {
  Equivalence::new(|a, b| a == b)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod predicate_ref {
    use super::*;

    #[test]
    fn new_creates_predicate() {
      let is_positive = PredicateRef::new(|x: &i32| *x > 0);
      assert!(is_positive.test(&5));
      assert!(!is_positive.test(&-3));
    }

    #[test]
    fn not_negates() {
      let is_positive = PredicateRef::new(|x: &i32| *x > 0);
      let is_not_positive = is_positive.not();
      assert!(!is_not_positive.test(&5));
      assert!(is_not_positive.test(&-3));
    }

    #[test]
    fn and_combines() {
      let is_positive = PredicateRef::new(|x: &i32| *x > 0);
      let is_small = PredicateRef::new(|x: &i32| *x < 10);
      let is_small_positive = is_positive.and(is_small);

      assert!(is_small_positive.test(&5));
      assert!(!is_small_positive.test(&15));
      assert!(!is_small_positive.test(&-3));
    }

    #[test]
    fn or_combines() {
      let is_negative = PredicateRef::new(|x: &i32| *x < 0);
      let is_large = PredicateRef::new(|x: &i32| *x > 100);
      let is_extreme = is_negative.or(is_large);

      assert!(is_extreme.test(&-5));
      assert!(is_extreme.test(&150));
      assert!(!is_extreme.test(&50));
    }

    #[rstest]
    #[case::positive(5, true)]
    #[case::zero(0, false)]
    #[case::negative(-3, false)]
    fn is_positive_cases(#[case] value: i32, #[case] expected: bool) {
      let pred = PredicateRef::new(|x: &i32| *x > 0);
      assert_eq!(pred.test(&value), expected);
    }
  }

  mod predicate_ref_contramap {
    use super::*;

    #[test]
    fn contramap_transforms_input() {
      // Predicate that checks if a string is short
      let is_short = PredicateRef::new(|s: &str| s.len() < 5);

      // Contramap to work on (String, i32) tuples by extracting the string
      #[allow(dead_code)]
      struct Pair(String, i32);
      let is_short_pair = is_short.contramap_ref(|p: &Pair| p.0.as_str());

      assert!(is_short_pair.test(&Pair("hi".to_string(), 42)));
      assert!(!is_short_pair.test(&Pair("hello world".to_string(), 42)));
    }
  }

  mod equivalence {
    use super::*;

    #[test]
    fn from_eq_uses_partial_eq() {
      let eq = from_eq::<i32>();
      assert!(eq.equivalent(&5, &5));
      assert!(!eq.equivalent(&5, &6));
    }

    #[test]
    fn custom_equivalence() {
      // Case-insensitive string equivalence
      let eq = Equivalence::new(|a: &String, b: &String| a.to_lowercase() == b.to_lowercase());

      assert!(eq.equivalent(&"Hello".to_string(), &"hello".to_string()));
      assert!(eq.equivalent(&"WORLD".to_string(), &"world".to_string()));
      assert!(!eq.equivalent(&"foo".to_string(), &"bar".to_string()));
    }

    #[test]
    fn contramap_transforms_input() {
      // Equivalence on lengths
      let len_eq = Equivalence::new(|a: &usize, b: &usize| a == b);

      // Contramap to work on strings by extracting length
      let string_len_eq = len_eq.contramap_with(|s: &String| s.len());

      assert!(string_len_eq.equivalent(&"hello".to_string(), &"world".to_string()));
      assert!(!string_len_eq.equivalent(&"hi".to_string(), &"hello".to_string()));
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn predicate_identity_law() {
      let pred = PredicateRef::new(|x: &i32| *x > 0);
      // contramap(id) should be equivalent to the original
      // We can't easily test this without a proper contramap on values,
      // but we can verify behavior is unchanged
      let contramapped = pred.clone().contramap_ref(|x: &i32| x);

      for val in [-5, 0, 5, 100] {
        assert_eq!(
          pred.test(&val),
          contramapped.test(&val),
          "identity law failed for {val}"
        );
      }
    }

    #[test]
    fn equivalence_identity_law() {
      let eq = from_eq::<i32>();
      let contramapped = eq.clone().contramap_with(|x: &i32| *x);

      for a in [0, 1, 5] {
        for b in [0, 1, 5] {
          assert_eq!(
            eq.equivalent(&a, &b),
            contramapped.equivalent(&a, &b),
            "identity law failed for ({a}, {b})"
          );
        }
      }
    }
  }
}
