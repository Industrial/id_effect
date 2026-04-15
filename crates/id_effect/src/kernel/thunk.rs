//! **Thunk** — a suspended computation.
//!
//! A thunk is a function from unit that produces a value when called.
//! It enables laziness: computation is deferred until needed.
//!
//! ## Definition
//!
//! ```text
//! THUNK[A] ::= () → A
//! ```
//!
//! ## Properties
//!
//! - Thunks defer computation until `force` is called
//! - Can be called multiple times (pure) or at most once (one-shot)
//! - Foundation for lazy evaluation in the effect system
//!
//! ## Relationship to Stratum 0 & 1
//!
//! - Uses: [`Unit`](super::super::foundation::unit) — thunks are functions from unit
//! - Uses: [`compose`](super::super::foundation::function::compose) — thunk composition

use crate::foundation::unit::Unit;

// ── Types ───────────────────────────────────────────────────────────────────

/// A one-shot thunk: can be forced exactly once.
///
/// This is the common case for effectful computations where the
/// suspended computation may have side effects or consume resources.
pub type Thunk<A> = Box<dyn FnOnce() -> A>;

/// A reusable thunk: can be forced multiple times.
///
/// Use this for pure computations where repeated evaluation
/// produces the same result.
pub type ThunkFn<A> = Box<dyn Fn() -> A>;

// ── Constructors ────────────────────────────────────────────────────────────

/// Create a one-shot thunk from a closure.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::thunk::{thunk, force};
///
/// let suspended = thunk(|| 42);
/// assert_eq!(force(suspended), 42);
/// ```
#[inline]
pub fn thunk<A, F>(f: F) -> Thunk<A>
where
  F: FnOnce() -> A + 'static,
{
  Box::new(f)
}

/// Create a thunk that immediately returns a value (strict/eager).
///
/// This is equivalent to `thunk(|| value)` but makes the intent clear.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::thunk::{strict, force};
///
/// let t = strict(42);
/// assert_eq!(force(t), 42);
/// ```
#[inline]
pub fn strict<A: 'static>(value: A) -> Thunk<A> {
  thunk(move || value)
}

/// Create a reusable thunk from a closure.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::thunk::{thunk_fn, force_fn};
///
/// let counter_factory = thunk_fn(|| 0);
/// assert_eq!(force_fn(&counter_factory), 0);
/// assert_eq!(force_fn(&counter_factory), 0); // Can force multiple times
/// ```
#[inline]
pub fn thunk_fn<A, F>(f: F) -> ThunkFn<A>
where
  F: Fn() -> A + 'static,
{
  Box::new(f)
}

// ── Combinators ─────────────────────────────────────────────────────────────

/// Force a one-shot thunk, producing its value.
///
/// Consumes the thunk (it can only be forced once).
#[inline]
pub fn force<A>(t: Thunk<A>) -> A {
  t()
}

/// Force a reusable thunk, producing its value.
///
/// Does not consume the thunk (it can be forced again).
#[inline]
pub fn force_fn<A>(t: &ThunkFn<A>) -> A {
  t()
}

/// Map over a one-shot thunk.
///
/// Returns a new thunk that, when forced, forces the original
/// and applies the function.
///
/// # Example
///
/// ```rust
/// use id_effect::kernel::thunk::{thunk, map, force};
///
/// let t = thunk(|| 21);
/// let doubled = map(t, |n| n * 2);
/// assert_eq!(force(doubled), 42);
/// ```
#[inline]
pub fn map<A, B, F>(t: Thunk<A>, f: F) -> Thunk<B>
where
  A: 'static,
  B: 'static,
  F: FnOnce(A) -> B + 'static,
{
  thunk(move || f(force(t)))
}

/// Map over a reusable thunk.
#[inline]
pub fn map_fn<A, B, F>(t: ThunkFn<A>, f: F) -> ThunkFn<B>
where
  A: 'static,
  B: 'static,
  F: Fn(A) -> B + 'static,
{
  thunk_fn(move || f(force_fn(&t)))
}

/// Flat-map over a one-shot thunk.
///
/// Returns a thunk that, when forced, forces the original,
/// applies the function to get another thunk, and forces that.
#[inline]
pub fn flat_map<A, B, F>(t: Thunk<A>, f: F) -> Thunk<B>
where
  A: 'static,
  B: 'static,
  F: FnOnce(A) -> Thunk<B> + 'static,
{
  thunk(move || force(f(force(t))))
}

/// Combine two thunks, producing a thunk of pairs.
///
/// When forced, forces both thunks and returns their values as a tuple.
#[inline]
pub fn zip<A, B>(ta: Thunk<A>, tb: Thunk<B>) -> Thunk<(A, B)>
where
  A: 'static,
  B: 'static,
{
  thunk(move || (force(ta), force(tb)))
}

/// Discard the result of a thunk, returning unit.
///
/// Useful for thunks with side effects where the result is not needed.
#[inline]
pub fn void<A: 'static>(t: Thunk<A>) -> Thunk<Unit> {
  map(t, |_| ())
}

/// Sequence two thunks, discarding the first result.
#[inline]
pub fn and_then<A, B>(ta: Thunk<A>, tb: Thunk<B>) -> Thunk<B>
where
  A: 'static,
  B: 'static,
{
  flat_map(ta, move |_| tb)
}

/// Create a thunk that applies a function thunk to a value thunk.
#[inline]
pub fn ap<A, B, F>(tf: Thunk<F>, ta: Thunk<A>) -> Thunk<B>
where
  A: 'static,
  B: 'static,
  F: FnOnce(A) -> B + 'static,
{
  flat_map(tf, move |f| map(ta, f))
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;
  use std::cell::Cell;
  use std::rc::Rc;

  mod constructors {
    use super::*;

    #[test]
    fn thunk_defers_computation() {
      let called = Rc::new(Cell::new(false));
      let called_ref = Rc::clone(&called);
      let t = thunk(move || {
        called_ref.set(true);
        42
      });

      // Not yet called
      assert!(!called.get());

      // Force it
      let result = force(t);
      assert!(called.get());
      assert_eq!(result, 42);
    }

    #[test]
    fn strict_returns_value_immediately_when_forced() {
      let t = strict(42);
      assert_eq!(force(t), 42);
    }

    #[test]
    fn thunk_fn_can_be_forced_multiple_times() {
      let counter = Rc::new(Cell::new(0));
      let counter_ref = Rc::clone(&counter);
      let t = thunk_fn(move || {
        let val = counter_ref.get();
        counter_ref.set(val + 1);
        val
      });

      assert_eq!(force_fn(&t), 0);
      assert_eq!(force_fn(&t), 1);
      assert_eq!(force_fn(&t), 2);
    }
  }

  mod map_combinator {
    use super::*;

    #[test]
    fn map_transforms_result() {
      let t = thunk(|| 21);
      let doubled = map(t, |n| n * 2);
      assert_eq!(force(doubled), 42);
    }

    #[test]
    fn map_defers_computation() {
      let called = Rc::new(Cell::new(false));
      let called_ref = Rc::clone(&called);
      let t = thunk(move || {
        called_ref.set(true);
        21
      });
      let doubled = map(t, |n| n * 2);

      assert!(!called.get());
      let _ = force(doubled);
      assert!(called.get());
    }

    #[rstest]
    #[case::identity(5, |x| x, 5)]
    #[case::double(3, |x| x * 2, 6)]
    #[case::negate(7, |x: i32| -x, -7)]
    fn map_applies_function(#[case] input: i32, #[case] f: fn(i32) -> i32, #[case] expected: i32) {
      let t = thunk(move || input);
      let mapped = map(t, f);
      assert_eq!(force(mapped), expected);
    }
  }

  mod flat_map_combinator {
    use super::*;

    #[test]
    fn flat_map_sequences_thunks() {
      let t = thunk(|| 5);
      let result = flat_map(t, |n| thunk(move || n * 2));
      assert_eq!(force(result), 10);
    }

    #[test]
    fn flat_map_defers_both_computations() {
      let first_called = Rc::new(Cell::new(false));
      let second_called = Rc::new(Cell::new(false));

      let first_ref = Rc::clone(&first_called);
      let second_ref = Rc::clone(&second_called);

      let t = thunk(move || {
        first_ref.set(true);
        5
      });
      let chained = flat_map(t, move |n| {
        thunk(move || {
          second_ref.set(true);
          n * 2
        })
      });

      assert!(!first_called.get());
      assert!(!second_called.get());

      let result = force(chained);
      assert!(first_called.get());
      assert!(second_called.get());
      assert_eq!(result, 10);
    }
  }

  mod zip_combinator {
    use super::*;

    #[test]
    fn zip_combines_two_thunks() {
      let ta = thunk(|| 1);
      let tb = thunk(|| "hello");
      let zipped = zip(ta, tb);
      assert_eq!(force(zipped), (1, "hello"));
    }

    #[test]
    fn zip_forces_both_thunks() {
      let a_called = Rc::new(Cell::new(false));
      let b_called = Rc::new(Cell::new(false));

      let a_ref = Rc::clone(&a_called);
      let b_ref = Rc::clone(&b_called);

      let ta = thunk(move || {
        a_ref.set(true);
        1
      });
      let tb = thunk(move || {
        b_ref.set(true);
        2
      });
      let zipped = zip(ta, tb);

      assert!(!a_called.get());
      assert!(!b_called.get());

      let _ = force(zipped);
      assert!(a_called.get());
      assert!(b_called.get());
    }
  }

  mod void_combinator {
    use super::*;

    #[test]
    fn void_discards_result() {
      let t = thunk(|| 42);
      let voided = void(t);
      assert_eq!(force(voided), ());
    }

    #[test]
    fn void_still_runs_computation() {
      let called = Rc::new(Cell::new(false));
      let called_ref = Rc::clone(&called);
      let t = thunk(move || {
        called_ref.set(true);
        42
      });
      let voided = void(t);

      assert!(!called.get());
      let _ = force(voided);
      assert!(called.get());
    }
  }

  mod and_then_combinator {
    use super::*;

    #[test]
    fn and_then_sequences_discarding_first() {
      let ta = thunk(|| 1);
      let tb = thunk(|| 2);
      let sequenced = and_then(ta, tb);
      assert_eq!(force(sequenced), 2);
    }

    #[test]
    fn and_then_runs_first_for_side_effects() {
      let first_called = Rc::new(Cell::new(false));
      let first_ref = Rc::clone(&first_called);

      let ta = thunk(move || {
        first_ref.set(true);
        1
      });
      let tb = thunk(|| 2);
      let sequenced = and_then(ta, tb);

      let result = force(sequenced);
      assert!(first_called.get());
      assert_eq!(result, 2);
    }
  }

  mod ap_combinator {
    use super::*;

    #[test]
    fn ap_applies_function_thunk_to_value_thunk() {
      let tf: Thunk<fn(i32) -> i32> = thunk(|| (|x| x * 2) as fn(i32) -> i32);
      let ta = thunk(|| 21);
      let result = ap(tf, ta);
      assert_eq!(force(result), 42);
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn functor_identity_law() {
      // map(id)(t) = t
      let t = thunk(|| 42);
      let mapped = map(t, |x| x);
      assert_eq!(force(mapped), 42);
    }

    #[test]
    fn functor_composition_law() {
      // map(f . g) = map(f) . map(g)
      let f = |x: i32| x + 1;
      let g = |x: i32| x * 2;

      let t1 = thunk(|| 5);
      let left = map(t1, move |x| f(g(x)));

      let t2 = thunk(|| 5);
      let right = map(map(t2, g), f);

      assert_eq!(force(left), force(right));
    }

    #[test]
    fn monad_left_identity_law() {
      // flat_map(strict(a), f) = f(a)
      let a = 5;
      let f = |x: i32| thunk(move || x * 2);

      let left = flat_map(strict(a), f);
      let right = f(a);

      assert_eq!(force(left), force(right));
    }

    #[test]
    fn monad_right_identity_law() {
      // flat_map(t, strict) = t
      let t = thunk(|| 42);
      let result = flat_map(t, |x| strict(x));
      assert_eq!(force(result), 42);
    }
  }
}
