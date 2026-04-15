//! **Result** — computation that may succeed or fail.
//!
//! A result represents computation that may succeed with `A` or fail with `E`.
//! This module provides operations on `Result<A, E>` in terms of algebraic structures.
//!
//! ## Definition
//!
//! ```text
//! RESULT[A, E] ::= Success(A) | Failure(E)
//! ```
//!
//! **Note:** `Result<A, E>` is isomorphic to `Either<E, A>` (from foundation::coproduct)
//! but with semantic intent: `Ok` is success, `Err` is failure.
//!
//! ## Algebraic Structure
//!
//! - `Result<_, E>` is a **Functor** for fixed `E`
//! - `Result<_, E>` is an **Applicative** for fixed `E`
//! - `Result<_, E>` is a **Monad** for fixed `E`
//! - `Result<A, _>` is a **Bifunctor**
//!
//! ## Relationship to Stratum 0 & 1
//!
//! - Uses: [`Either`](super::super::foundation::coproduct) — `Result` is isomorphic to `Either`
//! - Uses: [`Functor`](super::super::algebra::functor) — `map` operation
//! - Uses: [`Applicative`](super::super::algebra::applicative) — `pure`, `ap`, `map2`
//! - Uses: [`Monad`](super::super::algebra::monad) — `flat_map`, `flatten`
//! - Uses: [`Bifunctor`](super::super::algebra::bifunctor) — `bimap`, `map_error`

use crate::algebra::bifunctor::Bifunctor;
use crate::algebra::monad::Monad;
use crate::foundation::coproduct as either;

// ── Re-exports from algebra ─────────────────────────────────────────────────

// The trait implementations for Result are in the algebra module.
// This module provides free functions and additional combinators.

// ── Constructors ────────────────────────────────────────────────────────────

/// Wrap a value as a successful result.
///
/// This is the `pure` operation for the Result monad.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::succeed;
///
/// let r: Result<i32, &str> = succeed(42);
/// assert_eq!(r, Ok(42));
/// ```
#[inline]
pub fn succeed<A, E>(a: A) -> Result<A, E> {
  Ok(a)
}

/// Alias for `succeed` — lift a pure value into Result.
#[inline]
pub fn pure<A, E>(a: A) -> Result<A, E> {
  succeed(a)
}

/// Create a failed result.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::fail;
///
/// let r: Result<i32, &str> = fail("error");
/// assert_eq!(r, Err("error"));
/// ```
#[inline]
pub fn fail<A, E>(e: E) -> Result<A, E> {
  Err(e)
}

// ── Functor Operations ──────────────────────────────────────────────────────

/// Map over the success value.
///
/// Uses the [`Functor`] instance for `Result`.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::{succeed, map};
///
/// let r: Result<i32, &str> = succeed(21);
/// assert_eq!(map(r, |n| n * 2), Ok(42));
/// ```
#[inline]
pub fn map<A, B, E>(ra: Result<A, E>, f: impl FnOnce(A) -> B) -> Result<B, E> {
  ra.map(f)
}

/// Replace the success value with a constant.
#[inline]
pub fn as_<A, B, E>(ra: Result<A, E>, b: B) -> Result<B, E> {
  map(ra, |_| b)
}

/// Discard the success value, returning unit.
#[inline]
pub fn void<A, E>(ra: Result<A, E>) -> Result<(), E> {
  as_(ra, ())
}

// ── Bifunctor Operations ────────────────────────────────────────────────────

/// Map over both success and error.
///
/// Uses the [`Bifunctor`] instance for `Result` (`First` = success `A`, `Second` = error `E`).
#[inline]
pub fn bimap<A, B, E, E2>(
  ra: Result<A, E>,
  f_ok: impl FnOnce(A) -> B,
  f_err: impl FnOnce(E) -> E2,
) -> Result<B, E2> {
  ra.bimap(f_ok, f_err)
}

/// Map over the error value.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::{fail, map_error};
///
/// let r: Result<i32, &str> = fail("error");
/// assert_eq!(map_error(r, |s| s.len()), Err(5));
/// ```
#[inline]
pub fn map_error<A, E, E2>(ra: Result<A, E>, f: impl FnOnce(E) -> E2) -> Result<A, E2> {
  ra.map_err(f)
}

// ── Monad Operations ────────────────────────────────────────────────────────

/// Sequentially compose two results.
///
/// If the first is `Ok(a)`, apply `f` to get the second result.
/// If the first is `Err(e)`, propagate the error.
///
/// Uses the [`Monad`] instance for `Result`.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::{succeed, fail, flat_map};
///
/// let r: Result<i32, &str> = succeed(5);
/// assert_eq!(flat_map(r, |n| Ok(n * 2)), Ok(10));
///
/// let r2: Result<i32, &str> = fail("error");
/// assert_eq!(flat_map(r2, |n| Ok(n * 2)), Err("error"));
/// ```
#[inline]
pub fn flat_map<A, B, E>(ra: Result<A, E>, f: impl FnOnce(A) -> Result<B, E>) -> Result<B, E> {
  ra.flat_map(f)
}

/// Flatten a nested result.
///
/// `flatten(Ok(Ok(a))) = Ok(a)`
/// `flatten(Ok(Err(e))) = Err(e)`
/// `flatten(Err(e)) = Err(e)`
#[inline]
pub fn flatten<A, E>(rra: Result<Result<A, E>, E>) -> Result<A, E> {
  flat_map(rra, |ra| ra)
}

// ── Error Handling ──────────────────────────────────────────────────────────

/// Recover from an error by providing a fallback computation.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::{fail, catch};
///
/// let r: Result<i32, &str> = fail("error");
/// let recovered = catch(r, |_| Ok::<i32, &str>(42));
/// assert_eq!(recovered, Ok(42));
/// ```
#[inline]
pub fn catch<A, E, E2>(ra: Result<A, E>, f: impl FnOnce(E) -> Result<A, E2>) -> Result<A, E2> {
  match ra {
    Ok(a) => Ok(a),
    Err(e) => f(e),
  }
}

/// Recover from any error with a pure fallback value.
#[inline]
pub fn catch_all<A, E>(ra: Result<A, E>, fallback: impl FnOnce(E) -> A) -> A {
  match ra {
    Ok(a) => a,
    Err(e) => fallback(e),
  }
}

/// Try `ra`, and if it fails, try `rb`.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::{fail, succeed, or_else};
///
/// let r1: Result<i32, &str> = fail("first error");
/// let r2: Result<i32, &str> = succeed(42);
/// assert_eq!(or_else(r1, || r2), Ok(42));
/// ```
#[inline]
pub fn or_else<A, E>(ra: Result<A, E>, rb: impl FnOnce() -> Result<A, E>) -> Result<A, E> {
  match ra {
    Ok(a) => Ok(a),
    Err(_) => rb(),
  }
}

/// Return `ra` if successful, otherwise return `default`.
#[inline]
pub fn get_or_else<A, E>(ra: Result<A, E>, default: impl FnOnce(E) -> A) -> A {
  catch_all(ra, default)
}

// ── Applicative Operations ──────────────────────────────────────────────────

/// Lift a binary function over two results.
///
/// Returns `Err` if either input is `Err` (first error wins).
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::{succeed, fail, map2};
///
/// let r1: Result<i32, &str> = succeed(10);
/// let r2: Result<i32, &str> = succeed(32);
/// assert_eq!(map2(r1, r2, |a, b| a + b), Ok(42));
/// ```
#[inline]
pub fn map2<A, B, C, E>(
  ra: Result<A, E>,
  rb: Result<B, E>,
  f: impl FnOnce(A, B) -> C,
) -> Result<C, E> {
  flat_map(ra, |a| map(rb, |b| f(a, b)))
}

/// Lift a ternary function over three results.
#[inline]
pub fn map3<A, B, C, D, E>(
  ra: Result<A, E>,
  rb: Result<B, E>,
  rc: Result<C, E>,
  f: impl FnOnce(A, B, C) -> D,
) -> Result<D, E> {
  flat_map(ra, |a| flat_map(rb, |b| map(rc, |c| f(a, b, c))))
}

/// Combine two results into a pair.
#[inline]
pub fn zip<A, B, E>(ra: Result<A, E>, rb: Result<B, E>) -> Result<(A, B), E> {
  map2(ra, rb, |a, b| (a, b))
}

/// Sequence two results, keeping only the first value.
#[inline]
pub fn zip_left<A, B, E>(ra: Result<A, E>, rb: Result<B, E>) -> Result<A, E> {
  map2(ra, rb, |a, _| a)
}

/// Sequence two results, keeping only the second value.
#[inline]
pub fn zip_right<A, B, E>(ra: Result<A, E>, rb: Result<B, E>) -> Result<B, E> {
  map2(ra, rb, |_, b| b)
}

// ── Traversal ───────────────────────────────────────────────────────────────

/// Traverse a vector, collecting results.
///
/// Returns `Err` on the first failure.
pub fn traverse<A, B, E>(items: Vec<A>, f: impl Fn(A) -> Result<B, E>) -> Result<Vec<B>, E> {
  items.into_iter().map(f).collect()
}

/// Sequence a vector of results into a result of vector.
pub fn sequence<A, E>(items: Vec<Result<A, E>>) -> Result<Vec<A>, E> {
  items.into_iter().collect()
}

// ── Conditional ─────────────────────────────────────────────────────────────

/// Execute `f` only if `cond` is true.
///
/// Returns `Ok(Some(a))` if condition is true and `f` succeeds.
/// Returns `Ok(None)` if condition is false.
/// Returns `Err(e)` if condition is true and `f` fails.
#[inline]
pub fn when<A, E>(cond: bool, f: impl FnOnce() -> Result<A, E>) -> Result<Option<A>, E> {
  if cond { f().map(Some) } else { Ok(None) }
}

/// Ensure a condition holds, or return an error.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::result::ensure;
///
/// let r = ensure(5 > 3, || "5 is not greater than 3");
/// assert_eq!(r, Ok(()));
///
/// let r2 = ensure(1 > 3, || "1 is not greater than 3");
/// assert_eq!(r2, Err("1 is not greater than 3"));
/// ```
#[inline]
pub fn ensure<E>(cond: bool, err: impl FnOnce() -> E) -> Result<(), E> {
  if cond { Ok(()) } else { Err(err()) }
}

// ── Conversion ──────────────────────────────────────────────────────────────

/// Convert an `Option<A>` to `Result<A, E>`, using `err` for `None`.
#[inline]
pub fn from_option<A, E>(opt: Option<A>, err: impl FnOnce() -> E) -> Result<A, E> {
  opt.ok_or_else(err)
}

/// Convert a `Result<A, E>` to `Option<A>`, discarding the error.
#[inline]
pub fn to_option<A, E>(ra: Result<A, E>) -> Option<A> {
  ra.ok()
}

/// Flip the result: `Ok(a) → Err(a)`, `Err(e) → Ok(e)`.
///
/// Uses the isomorphism with `Either`.
#[inline]
pub fn flip<A, E>(ra: Result<A, E>) -> Result<E, A> {
  either::flip(ra)
}

/// Merge when both types are the same: extract the inner value.
#[inline]
pub fn merge<A>(ra: Result<A, A>) -> A {
  either::merge(ra)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn succeed_creates_ok() {
      let r: Result<i32, &str> = succeed(42);
      assert_eq!(r, Ok(42));
    }

    #[test]
    fn fail_creates_err() {
      let r: Result<i32, &str> = fail("error");
      assert_eq!(r, Err("error"));
    }

    #[test]
    fn pure_is_succeed() {
      let r1: Result<i32, &str> = succeed(42);
      let r2: Result<i32, &str> = pure(42);
      assert_eq!(r1, r2);
    }
  }

  mod functor_operations {
    use super::*;

    #[test]
    fn map_transforms_ok() {
      let r: Result<i32, &str> = succeed(21);
      assert_eq!(map(r, |n| n * 2), Ok(42));
    }

    #[test]
    fn map_passes_through_err() {
      let r: Result<i32, &str> = fail("error");
      assert_eq!(map(r, |n| n * 2), Err("error"));
    }

    #[test]
    fn as_replaces_value() {
      let r: Result<i32, &str> = succeed(1);
      assert_eq!(as_(r, "replaced"), Ok("replaced"));
    }

    #[test]
    fn void_discards_value() {
      let r: Result<i32, &str> = succeed(42);
      assert_eq!(void(r), Ok(()));
    }
  }

  mod bifunctor_operations {
    use super::*;

    #[test]
    fn bimap_transforms_ok() {
      let r: Result<i32, &str> = succeed(21);
      assert_eq!(bimap(r, |n| n * 2, |e| e.len()), Ok(42));
    }

    #[test]
    fn bimap_transforms_err() {
      let r: Result<i32, &str> = fail("error");
      assert_eq!(bimap(r, |n| n * 2, |e| e.len()), Err(5));
    }

    #[test]
    fn map_error_transforms_err() {
      let r: Result<i32, &str> = fail("error");
      assert_eq!(map_error(r, |e| e.len()), Err(5));
    }

    #[test]
    fn map_error_passes_through_ok() {
      let r: Result<i32, &str> = succeed(42);
      assert_eq!(map_error(r, |e| e.len()), Ok(42));
    }
  }

  mod monad_operations {
    use super::*;

    #[test]
    fn flat_map_chains_ok() {
      let r: Result<i32, &str> = succeed(5);
      assert_eq!(flat_map(r, |n| Ok(n * 2)), Ok(10));
    }

    #[test]
    fn flat_map_propagates_first_err() {
      let r: Result<i32, &str> = fail("first");
      assert_eq!(flat_map(r, |n| Ok(n * 2)), Err("first"));
    }

    #[test]
    fn flat_map_propagates_second_err() {
      let r: Result<i32, &str> = succeed(5);
      assert_eq!(flat_map(r, |_| Err::<i32, &str>("second")), Err("second"));
    }

    #[test]
    fn flatten_ok_ok() {
      let r: Result<Result<i32, &str>, &str> = succeed(succeed(42));
      assert_eq!(flatten(r), Ok(42));
    }

    #[test]
    fn flatten_ok_err() {
      let r: Result<Result<i32, &str>, &str> = succeed(fail("inner"));
      assert_eq!(flatten(r), Err("inner"));
    }

    #[test]
    fn flatten_err() {
      let r: Result<Result<i32, &str>, &str> = fail("outer");
      assert_eq!(flatten(r), Err("outer"));
    }
  }

  mod error_handling {
    use super::*;

    #[test]
    fn catch_recovers_from_err() {
      let r: Result<i32, &str> = fail("error");
      assert_eq!(catch(r, |_| Ok::<i32, &str>(42)), Ok(42));
    }

    #[test]
    fn catch_passes_through_ok() {
      let r: Result<i32, &str> = succeed(42);
      assert_eq!(catch(r, |_| Ok::<i32, &str>(0)), Ok(42));
    }

    #[test]
    fn catch_all_extracts_value() {
      let r: Result<i32, &str> = succeed(42);
      assert_eq!(catch_all(r, |_| 0), 42);
    }

    #[test]
    fn catch_all_uses_fallback() {
      let r: Result<i32, &str> = fail("error");
      assert_eq!(catch_all(r, |_| 99), 99);
    }

    #[test]
    fn or_else_tries_alternative() {
      let r1: Result<i32, &str> = fail("first");
      assert_eq!(or_else(r1, || Ok(42)), Ok(42));
    }

    #[test]
    fn or_else_returns_first_on_success() {
      let r1: Result<i32, &str> = succeed(42);
      assert_eq!(or_else(r1, || Ok(0)), Ok(42));
    }
  }

  mod applicative_operations {
    use super::*;

    #[test]
    fn map2_combines_ok_ok() {
      let r1: Result<i32, &str> = succeed(10);
      let r2: Result<i32, &str> = succeed(32);
      assert_eq!(map2(r1, r2, |a, b| a + b), Ok(42));
    }

    #[rstest]
    #[case::first_err(fail("e1"), succeed(2), Err("e1"))]
    #[case::second_err(succeed(1), fail("e2"), Err("e2"))]
    fn map2_propagates_err(
      #[case] r1: Result<i32, &str>,
      #[case] r2: Result<i32, &str>,
      #[case] expected: Result<i32, &str>,
    ) {
      assert_eq!(map2(r1, r2, |a, b| a + b), expected);
    }

    #[test]
    fn zip_creates_pair() {
      let r1: Result<i32, &str> = succeed(1);
      let r2: Result<&str, &str> = succeed("a");
      assert_eq!(zip(r1, r2), Ok((1, "a")));
    }

    #[test]
    fn zip_left_keeps_first() {
      let r1: Result<i32, &str> = succeed(1);
      let r2: Result<i32, &str> = succeed(2);
      assert_eq!(zip_left(r1, r2), Ok(1));
    }

    #[test]
    fn zip_right_keeps_second() {
      let r1: Result<i32, &str> = succeed(1);
      let r2: Result<i32, &str> = succeed(2);
      assert_eq!(zip_right(r1, r2), Ok(2));
    }
  }

  mod traversal {
    use super::*;

    #[test]
    fn traverse_all_ok() {
      let items = vec![1, 2, 3];
      let result = traverse(items, |n| Ok::<_, &str>(n * 2));
      assert_eq!(result, Ok(vec![2, 4, 6]));
    }

    #[test]
    fn traverse_stops_on_first_err() {
      let items = vec![1, 2, 3];
      let result = traverse(items, |n| if n == 2 { Err("boom") } else { Ok(n) });
      assert_eq!(result, Err("boom"));
    }

    #[test]
    fn sequence_all_ok() {
      let items = vec![Ok(1), Ok(2), Ok(3)];
      assert_eq!(sequence::<i32, &str>(items), Ok(vec![1, 2, 3]));
    }

    #[test]
    fn sequence_stops_on_err() {
      let items: Vec<Result<i32, &str>> = vec![Ok(1), Err("boom"), Ok(3)];
      assert_eq!(sequence(items), Err("boom"));
    }
  }

  mod conditional {
    use super::*;

    #[test]
    fn when_true_executes() {
      let r = when(true, || Ok::<_, &str>(42));
      assert_eq!(r, Ok(Some(42)));
    }

    #[test]
    fn when_false_returns_none() {
      let r = when(false, || Ok::<_, &str>(42));
      assert_eq!(r, Ok(None));
    }

    #[test]
    fn ensure_true_succeeds() {
      assert_eq!(ensure(true, || "error"), Ok(()));
    }

    #[test]
    fn ensure_false_fails() {
      assert_eq!(ensure(false, || "error"), Err("error"));
    }
  }

  mod conversion {
    use super::*;

    #[test]
    fn from_option_some() {
      assert_eq!(from_option(Some(42), || "missing"), Ok(42));
    }

    #[test]
    fn from_option_none() {
      assert_eq!(from_option(None::<i32>, || "missing"), Err("missing"));
    }

    #[test]
    fn to_option_ok() {
      assert_eq!(to_option(Ok::<i32, &str>(42)), Some(42));
    }

    #[test]
    fn to_option_err() {
      assert_eq!(to_option(Err::<i32, &str>("e")), None);
    }

    #[test]
    fn flip_ok_to_err() {
      assert_eq!(flip(Ok::<i32, &str>(42)), Err(42));
    }

    #[test]
    fn flip_err_to_ok() {
      assert_eq!(flip(Err::<i32, &str>("e")), Ok("e"));
    }

    #[test]
    fn merge_ok() {
      assert_eq!(merge(Ok::<i32, i32>(42)), 42);
    }

    #[test]
    fn merge_err() {
      assert_eq!(merge(Err::<i32, i32>(99)), 99);
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn monad_left_identity() {
      // flat_map(pure(a), f) = f(a)
      let a = 5;
      let f = |x: i32| Ok::<_, &str>(x * 2);

      let left = flat_map(pure(a), f);
      let right = f(a);
      assert_eq!(left, right);
    }

    #[test]
    fn monad_right_identity() {
      // flat_map(ra, pure) = ra
      let ra: Result<i32, &str> = succeed(42);
      let result = flat_map(ra.clone(), pure);
      assert_eq!(result, ra);
    }

    #[test]
    fn monad_associativity() {
      // flat_map(flat_map(ra, f), g) = flat_map(ra, |a| flat_map(f(a), g))
      let ra: Result<i32, &str> = succeed(5);
      let f = |x: i32| Ok::<_, &str>(x + 1);
      let g = |x: i32| Ok::<_, &str>(x * 2);

      let left = flat_map(flat_map(ra.clone(), f), g);
      let right = flat_map(ra, |a| flat_map(f(a), g));
      assert_eq!(left, right);
    }

    #[test]
    fn functor_identity() {
      // map(ra, |x| x) = ra
      let ra: Result<i32, &str> = succeed(42);
      assert_eq!(map(ra.clone(), |x| x), ra);
    }

    #[test]
    fn functor_composition() {
      // map(map(ra, g), f) = map(ra, |x| f(g(x)))
      let ra: Result<i32, &str> = succeed(5);
      let f = |x: i32| x + 1;
      let g = |x: i32| x * 2;

      let left = map(map(ra.clone(), g), f);
      let right = map(ra, |x| f(g(x)));
      assert_eq!(left, right);
    }
  }
}
