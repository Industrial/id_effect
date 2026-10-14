//! **Applicative** — a functor with pure and apply operations.
//!
//! An applicative functor extends [`Functor`](super::functor::Functor) with
//! the ability to lift values into the context (`pure`) and apply functions
//! that are themselves in the context (`ap`).
//!
//! ## Definition
//!
//! ```text
//! APPLICATIVE[F] ::= (
//!   Functor[F],
//!   pure: A → F<A>,
//!   ap: F<A → B> → F<A> → F<B>
//! )
//! ```
//!
//! ## Laws
//!
//! - **Identity**: `ap(pure(id), fa) = fa`
//! - **Homomorphism**: `ap(pure(f), pure(a)) = pure(f(a))`
//! - **Interchange**: `ap(ff, pure(a)) = ap(pure(|f| f(a)), ff)`
//! - **Composition**: `ap(ap(ap(pure(compose), ff), fg), fa) = ap(ff, ap(fg, fa))`
//!
//! ## Examples in this system
//!
//! - `Option<A>` — `pure = Some`, `ap` applies if both are `Some`
//! - `Result<A, E>` — `pure = Ok`, `ap` applies if both are `Ok`
//! - `Vec<A>` — `pure = vec![a]`, `ap` is cartesian product
//! - `Effect<A, E, R>` — lifts values/functions into effectful context
//!
//! ## Relationship to Stratum 0 & 1
//!
//! - Extends: [`Functor`](super::functor::Functor)
//! - Uses: [`identity`](super::super::foundation::function::identity),
//!         [`compose`](super::super::foundation::function::compose) for laws
//! - Extended by: [`Monad`](super::monad)

use super::functor::Functor;

/// A functor with lifting and application capabilities.
///
/// # Laws
///
/// ```text
/// ap(pure(|x| x), fa) = fa                          // Identity
/// ap(pure(f), pure(a)) = pure(f(a))                 // Homomorphism
/// ```
pub trait Applicative: Functor {
  /// Lift a value into the applicative context.
  fn pure(a: Self::Inner) -> Self;

  /// Apply a wrapped function to a wrapped value.
  fn ap<B, F>(self, ff: Self::Output<F>) -> Self::Output<B>
  where
    F: FnOnce(Self::Inner) -> B;
}

/// Lift a value into an applicative context (free function).
#[inline]
pub fn pure<F: Applicative>(a: F::Inner) -> F {
  F::pure(a)
}

// ── Applicative Module Functions ─────────────────────────────────────────────

/// Applicative operations for `Option<A>`.
pub mod option {
  /// Lift a value into Option.
  #[inline]
  pub fn pure<A>(a: A) -> Option<A> {
    Some(a)
  }

  /// Apply a function in Option to a value in Option.
  #[inline]
  pub fn ap<A, B>(ff: Option<impl FnOnce(A) -> B>, fa: Option<A>) -> Option<B> {
    match (ff, fa) {
      (Some(f), Some(a)) => Some(f(a)),
      _ => None,
    }
  }

  /// Lift a binary function over two Options.
  #[inline]
  pub fn map2<A, B, C>(fa: Option<A>, fb: Option<B>, f: impl FnOnce(A, B) -> C) -> Option<C> {
    match (fa, fb) {
      (Some(a), Some(b)) => Some(f(a, b)),
      _ => None,
    }
  }

  /// Lift a ternary function over three Options.
  #[inline]
  pub fn map3<A, B, C, D>(
    fa: Option<A>,
    fb: Option<B>,
    fc: Option<C>,
    f: impl FnOnce(A, B, C) -> D,
  ) -> Option<D> {
    match (fa, fb, fc) {
      (Some(a), Some(b), Some(c)) => Some(f(a, b, c)),
      _ => None,
    }
  }

  /// Sequence two Options, returning the second.
  #[inline]
  pub fn zip_right<A, B>(fa: Option<A>, fb: Option<B>) -> Option<B> {
    match (fa, fb) {
      (Some(_), Some(b)) => Some(b),
      _ => None,
    }
  }

  /// Sequence two Options, returning the first.
  #[inline]
  pub fn zip_left<A, B>(fa: Option<A>, fb: Option<B>) -> Option<A> {
    match (fa, fb) {
      (Some(a), Some(_)) => Some(a),
      _ => None,
    }
  }

  /// Tuple the contents of two Options.
  #[inline]
  pub fn zip<A, B>(fa: Option<A>, fb: Option<B>) -> Option<(A, B)> {
    match (fa, fb) {
      (Some(a), Some(b)) => Some((a, b)),
      _ => None,
    }
  }

  /// Sequence a Vec of Options into an Option of Vec.
  pub fn sequence<A>(opts: Vec<Option<A>>) -> Option<Vec<A>> {
    let mut result = Vec::with_capacity(opts.len());
    for opt in opts {
      let a = opt?;
      result.push(a);
    }
    Some(result)
  }

  /// Traverse a Vec, applying a function that returns Option.
  pub fn traverse<A, B>(items: Vec<A>, f: impl FnMut(A) -> Option<B>) -> Option<Vec<B>> {
    items.into_iter().map(f).collect()
  }
}

/// Applicative operations for `Result<A, E>`.
pub mod result {
  /// Lift a value into Ok.
  #[inline]
  pub fn pure<A, E>(a: A) -> Result<A, E> {
    Ok(a)
  }

  /// Apply a function in Result to a value in Result.
  #[inline]
  pub fn ap<A, B, E>(ff: Result<impl FnOnce(A) -> B, E>, fa: Result<A, E>) -> Result<B, E> {
    match (ff, fa) {
      (Ok(f), Ok(a)) => Ok(f(a)),
      (Err(e), _) => Err(e),
      (_, Err(e)) => Err(e),
    }
  }

  /// Lift a binary function over two Results.
  #[inline]
  pub fn map2<A, B, C, E>(
    fa: Result<A, E>,
    fb: Result<B, E>,
    f: impl FnOnce(A, B) -> C,
  ) -> Result<C, E> {
    match (fa, fb) {
      (Ok(a), Ok(b)) => Ok(f(a, b)),
      (Err(e), _) => Err(e),
      (_, Err(e)) => Err(e),
    }
  }

  /// Tuple the contents of two Results.
  #[inline]
  pub fn zip<A, B, E>(fa: Result<A, E>, fb: Result<B, E>) -> Result<(A, B), E> {
    match (fa, fb) {
      (Ok(a), Ok(b)) => Ok((a, b)),
      (Err(e), _) => Err(e),
      (_, Err(e)) => Err(e),
    }
  }

  /// Sequence a Vec of Results into a Result of Vec.
  pub fn sequence<A, E>(results: Vec<Result<A, E>>) -> Result<Vec<A>, E> {
    let mut output = Vec::with_capacity(results.len());
    for result in results {
      let a = result?;
      output.push(a);
    }
    Ok(output)
  }

  /// Traverse a Vec, applying a function that returns Result.
  pub fn traverse<A, B, E>(items: Vec<A>, f: impl FnMut(A) -> Result<B, E>) -> Result<Vec<B>, E> {
    items.into_iter().map(f).collect()
  }
}

/// Applicative operations for `Vec<A>`.
pub mod vec {
  /// Lift a value into a singleton Vec.
  #[inline]
  pub fn pure<A>(a: A) -> Vec<A> {
    vec![a]
  }

  /// Apply all functions to all values (cartesian product).
  pub fn ap<A: Clone, B>(ff: Vec<impl Fn(A) -> B>, fa: Vec<A>) -> Vec<B> {
    let mut result = Vec::with_capacity(ff.len() * fa.len());
    for f in ff.iter() {
      for a in fa.iter() {
        result.push(f(a.clone()));
      }
    }
    result
  }

  /// Lift a binary function over two Vecs (cartesian product).
  pub fn map2<A: Clone, B: Clone, C>(fa: Vec<A>, fb: Vec<B>, f: impl Fn(A, B) -> C) -> Vec<C> {
    let mut result = Vec::with_capacity(fa.len() * fb.len());
    for a in fa.iter() {
      for b in fb.iter() {
        result.push(f(a.clone(), b.clone()));
      }
    }
    result
  }

  /// Zip two Vecs element-wise (truncates to shorter length).
  pub fn zip_with<A, B, C>(fa: Vec<A>, fb: Vec<B>, f: impl Fn(A, B) -> C) -> Vec<C> {
    fa.into_iter().zip(fb).map(|(a, b)| f(a, b)).collect()
  }
}

// ── Trait Implementations ────────────────────────────────────────────────────

impl<A> Applicative for Option<A> {
  fn pure(a: A) -> Self {
    Some(a)
  }

  fn ap<B, F>(self, ff: Option<F>) -> Option<B>
  where
    F: FnOnce(A) -> B,
  {
    match (ff, self) {
      (Some(f), Some(a)) => Some(f(a)),
      _ => None,
    }
  }
}

impl<A, E> Applicative for Result<A, E> {
  fn pure(a: A) -> Self {
    Ok(a)
  }

  fn ap<B, F>(self, ff: Result<F, E>) -> Result<B, E>
  where
    F: FnOnce(A) -> B,
  {
    match (ff, self) {
      (Ok(f), Ok(a)) => Ok(f(a)),
      (Err(e), _) => Err(e),
      (_, Err(e)) => Err(e),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod pure_free_fn {
    use super::*;

    #[test]
    fn pure_lifts_value_into_option() {
      let result: Option<i32> = pure(42);
      assert_eq!(result, Some(42));
    }

    #[test]
    fn pure_lifts_value_into_result() {
      let result: Result<i32, &str> = pure(7);
      assert_eq!(result, Ok(7));
    }
  }

  mod option_applicative {
    use super::*;

    fn double(x: i32) -> i32 {
      x * 2
    }
    fn add2(a: i32, b: i32) -> i32 {
      a + b
    }
    fn sum3(a: i32, b: i32, c: i32) -> i32 {
      a + b + c
    }

    #[test]
    fn option_applicative_smoke() {
      assert_eq!(option::pure(42), Some(42));
      assert_eq!(option::ap(Some(double), Some(5)), Some(10));
      assert_eq!(option::ap(None::<fn(i32) -> i32>, Some(5)), None);
      assert_eq!(option::ap(Some(double), None), None);
      assert_eq!(option::map2(Some(2), Some(3), add2), Some(5));
      assert_eq!(option::map2(None::<i32>, Some(3), add2), None);
      assert_eq!(option::map2(Some(2), None::<i32>, add2), None);
      assert_eq!(option::map3(Some(1), Some(2), Some(3), sum3), Some(6));
      assert_eq!(option::map3(None, Some(2), Some(3), sum3), None);
      assert_eq!(option::zip_right(Some(1), Some("x")), Some("x"));
      assert_eq!(option::zip_right(None::<i32>, Some("x")), None);
      assert_eq!(option::zip_left(Some(1), Some("x")), Some(1));
      assert_eq!(option::zip_left(Some(1), None::<&str>), None);
      assert_eq!(option::zip(Some(1), Some("a")), Some((1, "a")));
      assert_eq!(option::zip(None::<i32>, Some("a")), None);
      assert_eq!(option::zip(Some(1), None::<&str>), None);
      assert_eq!(
        option::sequence(vec![Some(1), Some(2), Some(3)]),
        Some(vec![1, 2, 3])
      );
      assert_eq!(option::sequence(vec![Some(1), None, Some(3)]), None);
      assert_eq!(
        option::traverse(vec![1, 2, 3], |x| Some(double(x))),
        Some(vec![2, 4, 6])
      );
      assert_eq!(
        option::traverse(vec![1, 2, 3], |x| if x == 2 { None } else { Some(x) }),
        None
      );
    }
  }

  mod result_applicative {
    use super::*;

    fn double(x: i32) -> i32 {
      x * 2
    }
    fn add2(a: i32, b: i32) -> i32 {
      a + b
    }

    #[test]
    fn result_applicative_smoke() {
      assert_eq!(result::pure::<_, &str>(42), Ok(42));
      assert_eq!(
        result::ap(Ok::<_, &str>(double), Ok::<i32, &str>(5)),
        Ok(10)
      );
      assert_eq!(
        result::ap(Err::<fn(i32) -> i32, &str>("error"), Ok::<i32, &str>(5)),
        Err("error")
      );
      assert_eq!(
        result::ap(Ok::<_, &str>(double), Err("value error")),
        Err("value error")
      );
      assert_eq!(
        result::map2(Ok::<i32, &str>(2), Ok::<i32, &str>(3), add2),
        Ok(5)
      );
      assert_eq!(
        result::map2(Err::<i32, &str>("e1"), Ok::<i32, &str>(3), add2),
        Err("e1")
      );
      assert_eq!(
        result::map2(Ok::<i32, &str>(2), Err::<i32, &str>("e2"), add2),
        Err("e2")
      );
      assert_eq!(
        result::zip(Ok::<i32, &str>(1), Ok::<i32, &str>(2)),
        Ok((1, 2))
      );
      assert_eq!(
        result::zip(Err::<i32, &str>("e"), Ok::<i32, &str>(2)),
        Err("e")
      );
      assert_eq!(
        result::zip(Ok::<i32, &str>(1), Err::<i32, &str>("e2")),
        Err("e2")
      );
      assert_eq!(
        result::sequence(vec![Ok::<i32, &str>(1), Ok(2), Ok(3)]),
        Ok(vec![1, 2, 3])
      );
      assert_eq!(
        result::sequence(vec![Ok::<i32, &str>(1), Err("e1"), Err("e2")]),
        Err("e1")
      );
      assert_eq!(
        result::traverse(vec![1, 2, 3], |x| Ok::<i32, &str>(double(x))),
        Ok(vec![2, 4, 6])
      );
      assert_eq!(
        result::traverse(vec![1, 2, 3], |x| if x == 2 {
          Err::<i32, &str>("two")
        } else {
          Ok(x)
        }),
        Err("two")
      );
    }
  }

  mod vec_applicative {
    use super::*;

    fn inc(x: i32) -> i32 {
      x + 1
    }
    fn dbl(x: i32) -> i32 {
      x * 2
    }
    fn add(a: i32, b: i32) -> i32 {
      a + b
    }

    #[test]
    fn vec_applicative_smoke() {
      assert_eq!(vec::pure(42), vec![42]);
      assert_eq!(vec::ap(vec![inc, dbl], vec![1, 2]), vec![2, 3, 2, 4]);
      assert_eq!(
        vec::map2(vec![1, 2], vec![10, 20], add),
        vec![11, 21, 12, 22]
      );
      assert_eq!(
        vec::zip_with(vec![1, 2, 3], vec![10, 20, 30], add),
        vec![11, 22, 33]
      );
      assert_eq!(
        vec::zip_with(vec![1, 2], vec![10, 20, 30], add),
        vec![11, 22]
      );
    }
  }

  mod trait_impls {
    use super::*;

    fn inc(x: i32) -> i32 {
      x + 1
    }
    fn triple(x: i32) -> i32 {
      x * 3
    }

    #[test]
    fn trait_impls_smoke() {
      let opt: Option<i32> = Applicative::pure(5);
      assert_eq!(opt, Some(5));
      assert_eq!(Some(10_i32).ap(Some(inc)), Some(11));
      assert_eq!(None::<i32>.ap(Some(inc)), None);
      let res: Result<i32, &str> = Applicative::pure(5);
      assert_eq!(res, Ok(5));
      assert_eq!(Ok::<i32, &str>(5).ap(Ok::<_, &str>(triple)), Ok(15));
      assert_eq!(Err::<i32, &str>("e").ap(Ok::<_, &str>(triple)), Err("e"));
      assert_eq!(
        Ok::<i32, &str>(5).ap(Err::<fn(i32) -> i32, &str>("ff-err")),
        Err("ff-err")
      );
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn option_identity_law() {
      // ap(pure(id), fa) = fa
      let fa = Some(42);
      let result = option::ap(Some(|x: i32| x), fa.clone());
      assert_eq!(result, fa);
    }

    #[test]
    fn option_homomorphism_law() {
      // ap(pure(f), pure(a)) = pure(f(a))
      let f = |x: i32| x * 2;
      let a = 5;

      let left = option::ap(Some(f), Some(a));
      let right = Some(f(a));
      assert_eq!(left, right);
    }

    #[test]
    fn result_identity_law() {
      let fa: Result<i32, &str> = Ok(42);
      let result = result::ap(Ok(|x: i32| x), fa.clone());
      assert_eq!(result, fa);
    }

    #[test]
    fn result_homomorphism_law() {
      let f = |x: i32| x * 2;
      let a = 5;

      let left: Result<i32, &str> = result::ap(Ok(f), Ok(a));
      let right: Result<i32, &str> = Ok(f(a));
      assert_eq!(left, right);
    }

    #[test]
    fn vec_product_and_map2_smoke() {
      use super::super::{option, pure as lift, result, vec};
      assert_eq!(lift::<Option<_>>(7), Some(7));
      assert_eq!(lift::<Result<_, &str>>(7), Ok(7));
      assert_eq!(option::pure(3), Some(3));
      assert_eq!(option::ap(Some(|x: i32| x + 1), Some(2)), Some(3));
      assert_eq!(result::pure::<i32, &str>(9), Ok(9));
      assert_eq!(
        result::ap(Ok::<_, &str>(|x: i32| x * 2), Ok::<i32, &str>(3)),
        Ok(6)
      );
      assert_eq!(vec::pure(7), vec![7]);
      assert_eq!(
        vec::map2(vec![1, 2], vec![3, 4], |a, b| a + b),
        vec![4, 5, 5, 6]
      );
      assert_eq!(
        vec::zip_with(vec![1, 2], vec![3, 4], |a, b| a + b),
        vec![4, 6]
      );
      assert_eq!(vec::ap(vec![|x: i32| x + 1], vec![1, 2]), vec![2, 3]);
      assert_eq!(
        option::map3(Some(1), Some(2), Some(3), |a, b, c| a + b + c),
        Some(6)
      );
      assert_eq!(option::zip_right(Some(1), Some(2)), Some(2));
      assert_eq!(option::zip_left(Some(1), Some(2)), Some(1));
      assert_eq!(option::zip(Some(1), Some(2)), Some((1, 2)));
      assert_eq!(option::sequence(vec![Some(1), Some(2)]), Some(vec![1, 2]));
      assert_eq!(
        option::traverse(vec![1, 2], |x| Some(x * 2)),
        Some(vec![2, 4])
      );
      assert_eq!(result::map2(Ok::<i32, &str>(1), Ok(2), |a, b| a + b), Ok(3));
      assert_eq!(
        result::zip(Ok::<i32, &str>(1), Ok::<i32, &str>(2)),
        Ok((1, 2))
      );
      assert_eq!(
        result::sequence(vec![Ok::<i32, &str>(1), Ok(2)]),
        Ok(vec![1, 2])
      );
      assert_eq!(
        result::traverse(vec![1, 2], |x| Ok::<i32, &str>(x * 2)),
        Ok(vec![2, 4])
      );
    }

    #[rstest]
    #[case::some_value(Some(5))]
    #[case::none(None)]
    fn option_identity_parametric(#[case] fa: Option<i32>) {
      let result = option::ap(Some(|x: i32| x), fa.clone());
      assert_eq!(result, fa);
    }
  }
}
