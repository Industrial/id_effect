//! **Reader** — computation that requires an environment.
//!
//! A reader is a function from an environment `R` to a value `A`.
//! It enables dependency injection and environment threading.
//!
//! ## Definition
//!
//! ```text
//! READER[A, R] ::= R → A
//! ```
//!
//! ## Algebraic Structure
//!
//! - `Reader<_, R>` is a **Functor**
//! - `Reader<_, R>` is a **Applicative**
//! - `Reader<_, R>` is a **Monad**
//! - `Reader<A, _>` is **Contravariant** in `R`
//!
//! ## Operations
//!
//! - `ask` — Access the environment
//! - `local` — Modify the environment locally
//! - `run` — Execute with a specific environment
//!
//! ## Relationship to Stratum 0 & 1
//!
//! - Uses: [`identity`](super::super::foundation::function::identity) — `ask` is identity
//! - Uses: [`compose`](super::super::foundation::function::compose) — `local` uses precomposition
//! - Uses: [`Functor`](super::super::algebra::functor) — `map` postcomposes
//! - Uses: [`Monad`](super::super::algebra::monad) — `flat_map` sequences readers

use crate::foundation::function;

// ── Types ───────────────────────────────────────────────────────────────────

/// A reader: a function from environment `R` to value `A`.
///
/// This is a simple wrapper around a boxed closure to enable
/// method chaining and the monad interface.
pub struct Reader<A, R> {
  run: Box<dyn FnOnce(R) -> A>,
}

/// A reusable reader that can be run multiple times (requires `Fn`).
pub struct ReaderFn<A, R> {
  run: Box<dyn Fn(&R) -> A>,
}

// ── Constructors ────────────────────────────────────────────────────────────

/// Create a reader from a function.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::reader::{reader, run};
///
/// let r = reader(|env: i32| env * 2);
/// assert_eq!(run(r, 21), 42);
/// ```
#[inline]
pub fn reader<A, R, F>(f: F) -> Reader<A, R>
where
  F: FnOnce(R) -> A + 'static,
{
  Reader { run: Box::new(f) }
}

/// Create a reusable reader from a function.
#[inline]
pub fn reader_fn<A, R, F>(f: F) -> ReaderFn<A, R>
where
  F: Fn(&R) -> A + 'static,
{
  ReaderFn { run: Box::new(f) }
}

/// A reader that returns the environment unchanged.
///
/// This is the `ask` operation — access the current environment.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::reader::{ask, run};
///
/// let r = ask::<String>();
/// assert_eq!(run(r, "hello".to_string()), "hello");
/// ```
#[inline]
pub fn ask<R: 'static>() -> Reader<R, R> {
  reader(function::identity)
}

/// A reader that returns a constant value, ignoring the environment.
///
/// This is the `pure` operation for the Reader monad.
#[inline]
pub fn pure<A: 'static, R: 'static>(a: A) -> Reader<A, R> {
  reader(move |_| a)
}

/// Alias for `pure`.
#[inline]
pub fn succeed<A: 'static, R: 'static>(a: A) -> Reader<A, R> {
  pure(a)
}

// ── Running ─────────────────────────────────────────────────────────────────

/// Run a reader with the given environment.
#[inline]
pub fn run<A, R>(r: Reader<A, R>, env: R) -> A {
  (r.run)(env)
}

/// Run a reusable reader with the given environment.
#[inline]
pub fn run_fn<A, R>(r: &ReaderFn<A, R>, env: &R) -> A {
  (r.run)(env)
}

// ── Functor Operations ──────────────────────────────────────────────────────

/// Map over the result of a reader.
///
/// `map(r, f)` runs `r` and then applies `f` to the result.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::reader::{ask, map, run};
///
/// let r = map(ask::<i32>(), |n| n * 2);
/// assert_eq!(run(r, 21), 42);
/// ```
#[inline]
pub fn map<A, B, R, F>(r: Reader<A, R>, f: F) -> Reader<B, R>
where
  A: 'static,
  B: 'static,
  R: 'static,
  F: FnOnce(A) -> B + 'static,
{
  reader(move |env| f(run(r, env)))
}

/// Replace the result with a constant.
#[inline]
pub fn as_<A, B, R>(r: Reader<A, R>, b: B) -> Reader<B, R>
where
  A: 'static,
  B: 'static,
  R: 'static,
{
  map(r, move |_| b)
}

/// Discard the result, returning unit.
#[inline]
pub fn void<A: 'static, R: 'static>(r: Reader<A, R>) -> Reader<(), R> {
  as_(r, ())
}

// ── Monad Operations ────────────────────────────────────────────────────────

/// Sequentially compose two readers.
///
/// `flat_map(r, f)` runs `r`, passes the result to `f` to get another reader,
/// and runs that reader with the same environment.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::reader::{ask, flat_map, pure, run};
///
/// let r = flat_map(ask::<i32>(), |n| pure(n * 2));
/// assert_eq!(run(r, 21), 42);
/// ```
#[inline]
pub fn flat_map<A, B, R, F>(r: Reader<A, R>, f: F) -> Reader<B, R>
where
  A: 'static,
  B: 'static,
  R: Clone + 'static,
  F: FnOnce(A) -> Reader<B, R> + 'static,
{
  reader(move |env: R| {
    let a = run(r, env.clone());
    run(f(a), env)
  })
}

/// Flatten a nested reader.
#[inline]
pub fn flatten<A, R>(rr: Reader<Reader<A, R>, R>) -> Reader<A, R>
where
  A: 'static,
  R: Clone + 'static,
{
  flat_map(rr, |r| r)
}

// ── Environment Operations ──────────────────────────────────────────────────

/// Modify the environment before running a reader.
///
/// `local(f, r)` runs `r` with `f(env)` instead of `env`.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::reader::{ask, local, run};
///
/// let r = local(|n: i32| n * 2, ask::<i32>());
/// assert_eq!(run(r, 21), 42);
/// ```
#[inline]
pub fn local<A, R, F>(f: F, r: Reader<A, R>) -> Reader<A, R>
where
  A: 'static,
  R: 'static,
  F: FnOnce(R) -> R + 'static,
{
  reader(move |env| run(r, f(env)))
}

/// Provide a fixed environment, eliminating the environment parameter.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::reader::{ask, provide, run};
///
/// let r = provide(ask::<i32>(), 42);
/// assert_eq!(run(r, ()), 42);  // Environment is ignored
/// ```
#[inline]
pub fn provide<A, R>(r: Reader<A, R>, env: R) -> Reader<A, ()>
where
  A: 'static,
  R: 'static,
{
  reader(move |_: ()| run(r, env))
}

/// Project a smaller environment from a larger one.
///
/// Useful when a reader requires only part of the available environment.
#[inline]
pub fn asks<A, R, S, F>(f: F, r: Reader<A, S>) -> Reader<A, R>
where
  A: 'static,
  R: 'static,
  S: 'static,
  F: FnOnce(R) -> S + 'static,
{
  reader(move |env: R| run(r, f(env)))
}

// ── Applicative Operations ──────────────────────────────────────────────────

/// Combine two readers with a binary function.
#[inline]
pub fn map2<A, B, C, R, F>(ra: Reader<A, R>, rb: Reader<B, R>, f: F) -> Reader<C, R>
where
  A: 'static,
  B: 'static,
  C: 'static,
  R: Clone + 'static,
  F: FnOnce(A, B) -> C + 'static,
{
  flat_map(ra, move |a| map(rb, move |b| f(a, b)))
}

/// Combine two readers into a pair.
#[inline]
pub fn zip<A, B, R>(ra: Reader<A, R>, rb: Reader<B, R>) -> Reader<(A, B), R>
where
  A: 'static,
  B: 'static,
  R: Clone + 'static,
{
  map2(ra, rb, |a, b| (a, b))
}

/// Sequence two readers, keeping the first result.
#[inline]
pub fn zip_left<A, B, R>(ra: Reader<A, R>, rb: Reader<B, R>) -> Reader<A, R>
where
  A: 'static,
  B: 'static,
  R: Clone + 'static,
{
  map2(ra, rb, |a, _| a)
}

/// Sequence two readers, keeping the second result.
#[inline]
pub fn zip_right<A, B, R>(ra: Reader<A, R>, rb: Reader<B, R>) -> Reader<B, R>
where
  A: 'static,
  B: 'static,
  R: Clone + 'static,
{
  map2(ra, rb, |_, b| b)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn reader_creates_from_function() {
      let r = reader(|env: i32| env * 2);
      assert_eq!(run(r, 21), 42);
    }

    #[test]
    fn ask_returns_environment() {
      let r = ask::<String>();
      assert_eq!(run(r, "hello".to_string()), "hello");
    }

    #[test]
    fn pure_ignores_environment() {
      let r = pure::<_, i32>(42);
      assert_eq!(run(r, 999), 42);
    }

    #[test]
    fn succeed_is_pure() {
      let r = succeed::<_, i32>(42);
      assert_eq!(run(r, 999), 42);
    }
  }

  mod functor_operations {
    use super::*;

    #[test]
    fn map_transforms_result() {
      let r = map(ask::<i32>(), |n| n * 2);
      assert_eq!(run(r, 21), 42);
    }

    #[test]
    fn as_replaces_result() {
      let r = as_(ask::<i32>(), "replaced");
      assert_eq!(run(r, 21), "replaced");
    }

    #[test]
    fn void_discards_result() {
      let r = void(ask::<i32>());
      assert_eq!(run(r, 21), ());
    }

    #[rstest]
    #[case::identity(5, |x| x, 5)]
    #[case::double(3, |x| x * 2, 6)]
    #[case::negate(7, |x: i32| -x, -7)]
    fn map_applies_function(#[case] env: i32, #[case] f: fn(i32) -> i32, #[case] expected: i32) {
      let r = map(ask::<i32>(), f);
      assert_eq!(run(r, env), expected);
    }
  }

  mod monad_operations {
    use super::*;

    #[test]
    fn flat_map_sequences_readers() {
      let r = flat_map(ask::<i32>(), |n| pure(n * 2));
      assert_eq!(run(r, 21), 42);
    }

    #[test]
    fn flat_map_shares_environment() {
      // Both readers see the same environment
      let r = flat_map(ask::<i32>(), |n| map(ask::<i32>(), move |m| n + m));
      assert_eq!(run(r, 10), 20); // 10 + 10
    }

    #[test]
    fn flatten_unwraps_nested_reader() {
      let r = pure::<_, i32>(pure::<_, i32>(42));
      let flattened = flatten(r);
      assert_eq!(run(flattened, 0), 42);
    }
  }

  mod environment_operations {
    use super::*;

    #[test]
    fn local_modifies_environment() {
      let r = local(|n: i32| n * 2, ask::<i32>());
      assert_eq!(run(r, 21), 42);
    }

    #[test]
    fn local_only_affects_inner_reader() {
      let inner = local(|n: i32| n * 2, ask::<i32>());
      let outer = flat_map(ask::<i32>(), move |n| map(inner, move |m| (n, m)));
      // Outer sees 10, inner sees 10 * 2 = 20
      assert_eq!(run(outer, 10), (10, 20));
    }

    #[test]
    fn provide_fixes_environment() {
      let r = provide(ask::<i32>(), 42);
      assert_eq!(run(r, ()), 42);
    }

    #[test]
    fn asks_projects_environment() {
      #[derive(Clone)]
      struct Config {
        value: i32,
      }

      let r = asks(|c: Config| c.value, ask::<i32>());
      assert_eq!(run(r, Config { value: 42 }), 42);
    }
  }

  mod applicative_operations {
    use super::*;

    #[test]
    fn map2_combines_readers() {
      let ra = ask::<i32>();
      let rb = map(ask::<i32>(), |n| n * 2);
      let combined = map2(ra, rb, |a, b| a + b);
      // With env=10: a=10, b=20, result=30
      assert_eq!(run(combined, 10), 30);
    }

    #[test]
    fn zip_creates_pair() {
      let ra = ask::<i32>();
      let rb = map(ask::<i32>(), |n| format!("{n}"));
      let zipped = zip(ra, rb);
      assert_eq!(run(zipped, 42), (42, "42".to_string()));
    }

    #[test]
    fn zip_left_keeps_first() {
      let ra = pure::<_, i32>(1);
      let rb = pure::<_, i32>(2);
      assert_eq!(run(zip_left(ra, rb), 0), 1);
    }

    #[test]
    fn zip_right_keeps_second() {
      let ra = pure::<_, i32>(1);
      let rb = pure::<_, i32>(2);
      assert_eq!(run(zip_right(ra, rb), 0), 2);
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn functor_identity() {
      // map(r, id) = r
      let r = ask::<i32>();
      let mapped = map(r, |x| x);
      assert_eq!(run(mapped, 42), 42);
    }

    #[test]
    fn functor_composition() {
      // map(map(r, g), f) = map(r, f . g)
      let f = |x: i32| x + 1;
      let g = |x: i32| x * 2;

      let r1 = ask::<i32>();
      let left = map(map(r1, g), f);

      let r2 = ask::<i32>();
      let right = map(r2, move |x| f(g(x)));

      assert_eq!(run(left, 5), run(right, 5));
    }

    #[test]
    fn monad_left_identity() {
      // flat_map(pure(a), f) = f(a)
      let a = 5;
      let f = |x: i32| pure::<_, i32>(x * 2);

      let left = flat_map(pure(a), f);
      let right = f(a);

      assert_eq!(run(left, 0), run(right, 0));
    }

    #[test]
    fn monad_right_identity() {
      // flat_map(r, pure) = r
      let r = ask::<i32>();
      let result = flat_map(r, |x| pure(x));
      assert_eq!(run(result, 42), 42);
    }

    #[test]
    fn monad_associativity() {
      // flat_map(flat_map(r, f), g) = flat_map(r, |a| flat_map(f(a), g))
      let f = |x: i32| pure::<_, i32>(x + 1);
      let g = |x: i32| pure::<_, i32>(x * 2);

      let r1 = ask::<i32>();
      let left = flat_map(flat_map(r1, f), g);

      let r2 = ask::<i32>();
      let right = flat_map(r2, move |a| flat_map(f(a), g));

      assert_eq!(run(left, 5), run(right, 5));
    }
  }

  mod reader_fn_reusable {
    use super::*;

    #[test]
    fn reader_fn_can_be_run_multiple_times() {
      let r = reader_fn(|env: &i32| *env * 2);
      assert_eq!(run_fn(&r, &21), 42);
      assert_eq!(run_fn(&r, &10), 20);
      assert_eq!(run_fn(&r, &5), 10);
    }
  }
}
