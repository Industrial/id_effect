//! **Monad** — an applicative with sequential composition.
//!
//! A monad extends [`Applicative`](super::applicative::Applicative) with
//! `flat_map` (also called `bind` or `>>=`), which allows sequencing
//! computations where each step can depend on the result of the previous.
//!
//! ## Definition
//!
//! ```text
//! MONAD[F] ::= (
//!   Applicative[F],
//!   flat_map: F<A> → (A → F<B>) → F<B>
//! )
//! ```
//!
//! ## Laws
//!
//! - **Left identity**: `flat_map(pure(a), f) = f(a)`
//! - **Right identity**: `flat_map(fa, pure) = fa`
//! - **Associativity**: `flat_map(flat_map(fa, f), g) = flat_map(fa, |a| flat_map(f(a), g))`
//!
//! ## Examples in this system
//!
//! - `Option<A>` — chains computations that may fail
//! - `Result<A, E>` — chains computations with typed errors
//! - `Vec<A>` — flattens nested lists (flat_map = concat_map)
//! - `Effect<A, E, R>` — sequences effectful computations
//!
//! ## Relationship to Stratum 0 & 1
//!
//! - Extends: [`Applicative`](super::applicative::Applicative)
//! - The monad laws ensure `flat_map` behaves like function composition
//!   but in the effectful context
//! - `Effect<A, E, R>` is the central monad of this effect system

use super::applicative::Applicative;

/// A monad: an applicative with sequential composition.
///
/// # Laws
///
/// ```text
/// flat_map(pure(a), f) = f(a)                       // Left identity
/// flat_map(fa, pure) = fa                           // Right identity
/// flat_map(flat_map(fa, f), g) = flat_map(fa, |a| flat_map(f(a), g))  // Associativity
/// ```
pub trait Monad: Applicative {
  /// Sequentially compose two computations, passing the result of the first
  /// to a function that produces the second.
  fn flat_map<B, F>(self, f: F) -> Self::Output<B>
  where
    F: FnOnce(Self::Inner) -> Self::Output<B>;

  /// Flatten a nested monadic structure.
  ///
  /// `flatten(mma) = flat_map(mma, |ma| ma)`
  fn flatten(self) -> Self::Inner
  where
    Self: Sized,
    Self::Inner: Sized,
  {
    // This default requires Inner to be the same as Output<Inner::Inner>,
    // which is hard to express in Rust. Implementors should override.
    unimplemented!("flatten requires specific implementation")
  }
}

/// Sequential composition (free function).
#[inline]
pub fn flat_map<M: Monad, B>(ma: M, f: impl FnOnce(M::Inner) -> M::Output<B>) -> M::Output<B> {
  ma.flat_map(f)
}

// ── Monad Module Functions ───────────────────────────────────────────────────

/// Monad operations for `Option<A>`.
pub mod option {
  pub use super::super::applicative::option::{pure, sequence, traverse};

  /// Chain Option computations.
  #[inline]
  pub fn flat_map<A, B>(fa: Option<A>, f: impl FnOnce(A) -> Option<B>) -> Option<B> {
    fa.and_then(f)
  }

  /// Flatten nested Options.
  #[inline]
  pub fn flatten<A>(mma: Option<Option<A>>) -> Option<A> {
    mma.flatten()
  }

  /// Execute two Options in sequence, ignoring the first result.
  #[inline]
  pub fn and_then_discard<A, B>(fa: Option<A>, fb: Option<B>) -> Option<B> {
    fa.and_then(|_| fb)
  }

  /// Chain with early return on None.
  #[inline]
  pub fn filter_map<A, B>(fa: Option<A>, f: impl FnOnce(A) -> Option<B>) -> Option<B> {
    fa.and_then(f)
  }

  /// Conditional execution: run `f` only if `cond` is true.
  #[inline]
  pub fn when<A>(cond: bool, f: impl FnOnce() -> Option<A>) -> Option<A> {
    if cond { f() } else { None }
  }

  /// Conditional execution: run `f` only if `cond` is false.
  #[inline]
  pub fn unless<A>(cond: bool, f: impl FnOnce() -> Option<A>) -> Option<A> {
    when(!cond, f)
  }

  /// Iterate while `f` returns `Some`.
  pub fn iterate<A: Clone>(init: A, f: impl Fn(&A) -> Option<A>) -> A {
    let mut current = init;
    while let Some(next) = f(&current) {
      current = next;
    }
    current
  }
}

/// Monad operations for `Result<A, E>`.
pub mod result {
  pub use super::super::applicative::result::{pure, sequence, traverse};

  /// Chain Result computations.
  #[inline]
  pub fn flat_map<A, B, E>(fa: Result<A, E>, f: impl FnOnce(A) -> Result<B, E>) -> Result<B, E> {
    fa.and_then(f)
  }

  /// Flatten nested Results.
  #[inline]
  pub fn flatten<A, E>(mma: Result<Result<A, E>, E>) -> Result<A, E> {
    mma.and_then(|x| x)
  }

  /// Chain two Results, ignoring the first Ok value.
  #[inline]
  pub fn and_then_discard<A, B, E>(fa: Result<A, E>, fb: Result<B, E>) -> Result<B, E> {
    fa.and_then(|_| fb)
  }

  /// Recover from an error with a fallback computation.
  #[inline]
  pub fn or_else<A, E, F>(fa: Result<A, E>, f: impl FnOnce(E) -> Result<A, F>) -> Result<A, F> {
    fa.or_else(f)
  }

  /// Map the error type.
  #[inline]
  pub fn map_err<A, E, F>(fa: Result<A, E>, f: impl FnOnce(E) -> F) -> Result<A, F> {
    fa.map_err(f)
  }

  /// Conditional execution: run `f` only if `cond` is true.
  #[inline]
  pub fn when<A, E>(cond: bool, f: impl FnOnce() -> Result<A, E>) -> Result<Option<A>, E> {
    if cond { f().map(Some) } else { Ok(None) }
  }

  /// Ensure a condition holds, or return an error.
  #[inline]
  pub fn ensure<E>(cond: bool, err: impl FnOnce() -> E) -> Result<(), E> {
    if cond { Ok(()) } else { Err(err()) }
  }
}

/// Monad operations for `Vec<A>`.
pub mod vec {
  pub use super::super::applicative::vec::pure;

  /// Chain Vec computations (concat_map / flat_map).
  pub fn flat_map<A, B>(fa: Vec<A>, f: impl FnMut(A) -> Vec<B>) -> Vec<B> {
    fa.into_iter().flat_map(f).collect()
  }

  /// Flatten nested Vecs.
  pub fn flatten<A>(mma: Vec<Vec<A>>) -> Vec<A> {
    mma.into_iter().flatten().collect()
  }

  /// Filter elements by a predicate.
  pub fn filter<A>(fa: Vec<A>, pred: impl FnMut(&A) -> bool) -> Vec<A> {
    fa.into_iter().filter(pred).collect()
  }

  /// Filter and map in one pass.
  pub fn filter_map<A, B>(fa: Vec<A>, f: impl FnMut(A) -> Option<B>) -> Vec<B> {
    fa.into_iter().filter_map(f).collect()
  }

  /// Replicate a value n times.
  pub fn replicate<A: Clone>(n: usize, a: A) -> Vec<A> {
    vec![a; n]
  }
}

// ── Trait Implementations ────────────────────────────────────────────────────

impl<A> Monad for Option<A> {
  fn flat_map<B, F>(self, f: F) -> Option<B>
  where
    F: FnOnce(A) -> Option<B>,
  {
    self.and_then(f)
  }
}

impl<A, E> Monad for Result<A, E> {
  fn flat_map<B, F>(self, f: F) -> Result<B, E>
  where
    F: FnOnce(A) -> Result<B, E>,
  {
    self.and_then(f)
  }
}

// ── Do-notation helpers ──────────────────────────────────────────────────────

/// Helper macro for monadic do-notation (limited form).
///
/// This provides a simple way to chain monadic operations without deeply nested closures.
///
/// # Example
///
/// ```rust
/// use id_effect::algebra::monad::option;
///
/// fn divide(a: i32, b: i32) -> Option<i32> {
///     if b == 0 { None } else { Some(a / b) }
/// }
///
/// let result = option::flat_map(divide(10, 2), |x|
///     option::flat_map(divide(x, 1), |y|
///         Some(y + 1)
///     )
/// );
///
/// assert_eq!(result, Some(6));
/// ```

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod option_monad {
    use super::*;

    #[test]
    fn flat_map_some_returns_some() {
      let result = option::flat_map(Some(5), |x| Some(x * 2));
      assert_eq!(result, Some(10));
    }

    #[test]
    fn flat_map_some_returns_none() {
      let result = option::flat_map(Some(5), |_| None::<i32>);
      assert_eq!(result, None);
    }

    #[test]
    fn flat_map_none_returns_none() {
      let result = option::flat_map(None::<i32>, |x| Some(x * 2));
      assert_eq!(result, None);
    }

    #[test]
    fn flatten_some_some() {
      assert_eq!(option::flatten(Some(Some(42))), Some(42));
    }

    #[test]
    fn flatten_some_none() {
      assert_eq!(option::flatten(Some(None::<i32>)), None);
    }

    #[test]
    fn flatten_none() {
      assert_eq!(option::flatten(None::<Option<i32>>), None);
    }

    #[test]
    fn when_true_executes() {
      assert_eq!(option::when(true, || Some(42)), Some(42));
    }

    #[test]
    fn when_false_returns_none() {
      assert_eq!(option::when(false, || Some(42)), None);
    }

    #[test]
    fn iterate_finds_fixed_point() {
      // Iterate x/2 until < 1
      let result = option::iterate(100, |x| if *x >= 2 { Some(x / 2) } else { None });
      assert_eq!(result, 1);
    }

    // ── previously uncovered ──────────────────────────────────────────────

    #[test]
    fn and_then_discard_some_some() {
      assert_eq!(option::and_then_discard(Some(1), Some(2)), Some(2));
    }

    #[test]
    fn and_then_discard_none() {
      assert_eq!(option::and_then_discard(None::<i32>, Some(2)), None);
    }

    #[test]
    fn filter_map_maps_and_filters() {
      assert_eq!(
        option::filter_map(Some(4), |x| if x > 2 { Some(x * 10) } else { None }),
        Some(40)
      );
      assert_eq!(
        option::filter_map(Some(1), |x| if x > 2 { Some(x * 10) } else { None }),
        None
      );
      assert_eq!(option::filter_map(None::<i32>, |x| Some(x)), None);
    }

    #[test]
    fn unless_false_executes() {
      assert_eq!(option::unless(false, || Some(99)), Some(99));
    }

    #[test]
    fn unless_true_returns_none() {
      assert_eq!(option::unless(true, || Some(99)), None);
    }
  }

  mod result_monad {
    use super::*;

    #[test]
    fn flat_map_ok_ok() {
      let result: Result<i32, &str> = result::flat_map(Ok(5), |x| Ok(x * 2));
      assert_eq!(result, Ok(10));
    }

    #[test]
    fn flat_map_ok_err() {
      let result: Result<i32, &str> = result::flat_map(Ok(5), |_| Err("failed"));
      assert_eq!(result, Err("failed"));
    }

    #[test]
    fn flat_map_err_ok() {
      let result: Result<i32, &str> = result::flat_map(Err("error"), |x: i32| Ok(x * 2));
      assert_eq!(result, Err("error"));
    }

    #[test]
    fn flatten_ok_ok() {
      let nested: Result<Result<i32, &str>, &str> = Ok(Ok(42));
      assert_eq!(result::flatten(nested), Ok(42));
    }

    #[test]
    fn flatten_ok_err() {
      let nested: Result<Result<i32, &str>, &str> = Ok(Err("inner"));
      assert_eq!(result::flatten(nested), Err("inner"));
    }

    #[test]
    fn flatten_err() {
      let nested: Result<Result<i32, &str>, &str> = Err("outer");
      assert_eq!(result::flatten(nested), Err("outer"));
    }

    #[test]
    fn or_else_recovers() {
      let result: Result<i32, &str> = Err("error");
      let recovered = result::or_else(result, |_| Ok::<i32, &str>(42));
      assert_eq!(recovered, Ok(42));
    }

    #[test]
    fn ensure_true() {
      assert_eq!(result::ensure(true, || "error"), Ok(()));
    }

    #[test]
    fn ensure_false() {
      assert_eq!(result::ensure(false, || "error"), Err("error"));
    }

    // ── previously uncovered ──────────────────────────────────────────────

    #[test]
    fn and_then_discard_ok_ok() {
      let r: Result<i32, &str> = result::and_then_discard(Ok(1), Ok(2));
      assert_eq!(r, Ok(2));
    }

    #[test]
    fn and_then_discard_err() {
      let r: Result<i32, &str> = result::and_then_discard(Err::<i32, _>("e"), Ok(2));
      assert_eq!(r, Err("e"));
    }

    #[test]
    fn map_err_transforms_error() {
      let r: Result<i32, &str> = Err("e");
      let mapped = result::map_err(r, |e| format!("wrapped: {e}"));
      assert_eq!(mapped, Err("wrapped: e".to_string()));
    }

    #[test]
    fn map_err_ok_unchanged() {
      let r: Result<i32, &str> = Ok(42);
      assert_eq!(result::map_err(r, |_| "new_err"), Ok(42));
    }

    #[test]
    fn when_true_returns_some_ok() {
      assert_eq!(result::when(true, || Ok::<i32, &str>(99)), Ok(Some(99)));
    }

    #[test]
    fn when_false_returns_none_ok() {
      assert_eq!(result::when::<i32, &str>(false, || Ok(99)), Ok(None));
    }

    #[test]
    fn when_true_propagates_err() {
      assert_eq!(result::when(true, || Err::<i32, &str>("bad")), Err("bad"));
    }
  }

  mod vec_monad {
    use super::*;

    #[test]
    fn flat_map_expands() {
      let result = vec::flat_map(vec![1, 2, 3], |x| vec![x, x * 10]);
      assert_eq!(result, vec![1, 10, 2, 20, 3, 30]);
    }

    #[test]
    fn flat_map_empty() {
      let result = vec::flat_map(vec![1, 2, 3], |_| Vec::<i32>::new());
      assert_eq!(result, Vec::<i32>::new());
    }

    #[test]
    fn flatten_nested() {
      let nested = vec![vec![1, 2], vec![3], vec![4, 5, 6]];
      assert_eq!(vec::flatten(nested), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn filter_removes_elements() {
      let result = vec::filter(vec![1, 2, 3, 4, 5], |x| x % 2 == 0);
      assert_eq!(result, vec![2, 4]);
    }

    #[test]
    fn filter_map_combined() {
      let result = vec::filter_map(vec![1, 2, 3, 4], |x| {
        if x % 2 == 0 { Some(x * 10) } else { None }
      });
      assert_eq!(result, vec![20, 40]);
    }

    #[test]
    fn replicate_creates_copies() {
      assert_eq!(vec::replicate(3, "x"), vec!["x", "x", "x"]);
    }
  }

  // ── free flat_map function ────────────────────────────────────────────────

  mod free_flat_map {
    use super::*;

    #[test]
    fn free_flat_map_on_option_some() {
      let result = flat_map(Some(5), |x| Some(x + 1));
      assert_eq!(result, Some(6));
    }

    #[test]
    fn free_flat_map_on_option_none() {
      let result = flat_map(None::<i32>, |x| Some(x + 1));
      assert_eq!(result, None);
    }

    #[test]
    fn free_flat_map_on_result_ok() {
      let r: Result<i32, &str> = Ok(10);
      assert_eq!(flat_map(r, |x| Ok::<_, &str>(x * 2)), Ok(20));
    }

    #[test]
    fn free_flat_map_on_result_err() {
      let r: Result<i32, &str> = Err("e");
      assert_eq!(flat_map(r, |x| Ok::<_, &str>(x * 2)), Err("e"));
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn option_left_identity() {
      // flat_map(pure(a), f) = f(a)
      let a = 5;
      let f = |x: i32| Some(x * 2);

      let left = option::flat_map(Some(a), f);
      let right = f(a);
      assert_eq!(left, right);
    }

    #[test]
    fn option_right_identity() {
      // flat_map(fa, pure) = fa
      let fa = Some(42);
      let result = option::flat_map(fa.clone(), Some);
      assert_eq!(result, fa);
    }

    #[test]
    fn option_associativity() {
      // flat_map(flat_map(fa, f), g) = flat_map(fa, |a| flat_map(f(a), g))
      let fa = Some(5);
      let f = |x: i32| Some(x + 1);
      let g = |x: i32| Some(x * 2);

      let left = option::flat_map(option::flat_map(fa, f), g);
      let right = option::flat_map(fa, |a| option::flat_map(f(a), g));
      assert_eq!(left, right);
    }

    #[test]
    fn result_left_identity() {
      let a = 5;
      let f = |x: i32| Ok::<_, &str>(x * 2);

      let left = result::flat_map(Ok(a), f);
      let right = f(a);
      assert_eq!(left, right);
    }

    #[test]
    fn result_right_identity() {
      let fa: Result<i32, &str> = Ok(42);
      let result = result::flat_map(fa.clone(), Ok);
      assert_eq!(result, fa);
    }

    #[test]
    fn result_associativity() {
      let fa: Result<i32, &str> = Ok(5);
      let f = |x: i32| Ok::<_, &str>(x + 1);
      let g = |x: i32| Ok::<_, &str>(x * 2);

      let left = result::flat_map(result::flat_map(fa.clone(), f), g);
      let right = result::flat_map(fa, |a| result::flat_map(f(a), g));
      assert_eq!(left, right);
    }

    #[rstest]
    #[case::some(Some(5))]
    #[case::none(None)]
    fn option_right_identity_parametric(#[case] fa: Option<i32>) {
      let result = option::flat_map(fa.clone(), Some);
      assert_eq!(result, fa);
    }
  }
}
