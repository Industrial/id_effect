//! `Either<R, L>` type alias and free-function combinators — mirrors Effect.ts `Either`.
//!
//! In Rust, `Either<R, L>` is simply `Result<R, L>` — the `Ok` variant corresponds
//! to Effect.ts `Right` (the "happy path") and `Err` corresponds to `Left`.
//!
//! This module provides the Effect.ts-named companion functions (`right`, `left`,
//! `map_left`, `flip`, `merge`, etc.) so Rust code can be written in the same style
//! as Effect.ts without needing to remember which Rust method maps to which concept.

// ── Type alias ────────────────────────────────────────────────────────────────

/// `Either<R, L>` — `Ok(R)` is Right (success), `Err(L)` is Left (failure/alternative).
///
/// This is a transparent alias over `std::result::Result<R, L>`.
pub type Either<R, L> = Result<R, L>;

// ── either module ─────────────────────────────────────────────────────────────

/// Constructors and combinators for `Either<R, L>` — mirrors Effect.ts `Either` namespace.
#[allow(clippy::module_inception)] // intentional `either::either::…` mirror of Effect.ts
pub mod either {
  use super::Either;

  // ── Constructors ──────────────────────────────────────────────────────────

  /// Wrap `r` as the Right (success) variant.  Mirrors Effect.ts `Either.right`.
  pub fn right<R, L>(r: R) -> Either<R, L> {
    Ok(r)
  }

  /// Wrap `l` as the Left (failure/alternative) variant.  Mirrors Effect.ts `Either.left`.
  pub fn left<R, L>(l: L) -> Either<R, L> {
    Err(l)
  }

  // ── Inspection ────────────────────────────────────────────────────────────

  /// True when `e` is `Right` (`Ok`).
  pub fn is_right<R, L>(e: &Either<R, L>) -> bool {
    e.is_ok()
  }

  /// True when `e` is `Left` (`Err`).
  pub fn is_left<R, L>(e: &Either<R, L>) -> bool {
    e.is_err()
  }

  // ── Transformations ───────────────────────────────────────────────────────

  /// Apply `f` to the Right value; pass Left through unchanged.
  pub fn map<R, R2, L>(e: Either<R, L>, f: impl FnOnce(R) -> R2) -> Either<R2, L> {
    e.map(f)
  }

  /// Apply `f` to the Left value; pass Right through unchanged.
  ///
  /// Mirrors Effect.ts `Either.mapLeft`.
  pub fn map_left<R, L, L2>(e: Either<R, L>, f: impl FnOnce(L) -> L2) -> Either<R, L2> {
    e.map_err(f)
  }

  /// Flat-map over the Right value.
  pub fn flat_map<R, R2, L>(e: Either<R, L>, f: impl FnOnce(R) -> Either<R2, L>) -> Either<R2, L> {
    e.and_then(f)
  }

  /// Flat-map over the Left value, replacing it with a new `Either`.
  pub fn flat_map_left<R, L, L2>(
    e: Either<R, L>,
    f: impl FnOnce(L) -> Either<R, L2>,
  ) -> Either<R, L2> {
    match e {
      Ok(r) => Ok(r),
      Err(l) => f(l),
    }
  }

  /// Swap Right and Left: `Right(r)` → `Left(r)`, `Left(l)` → `Right(l)`.
  ///
  /// Mirrors Effect.ts `Either.flip`.
  pub fn flip<R, L>(e: Either<R, L>) -> Either<L, R> {
    match e {
      Ok(r) => Err(r),
      Err(l) => Ok(l),
    }
  }

  // ── Extraction ────────────────────────────────────────────────────────────

  /// Return the Right value, or compute a default from the Left value.
  ///
  /// Mirrors Effect.ts `Either.getOrElse`.
  pub fn get_or_else<R, L>(e: Either<R, L>, default: impl FnOnce(L) -> R) -> R {
    e.unwrap_or_else(default)
  }

  /// Return `e` if it is Right, otherwise try `f` with the Left value.
  pub fn or_else<R, L, L2>(e: Either<R, L>, f: impl FnOnce(L) -> Either<R, L2>) -> Either<R, L2> {
    match e {
      Ok(r) => Ok(r),
      Err(l) => f(l),
    }
  }

  /// Merge when both variants have the same type: return the inner value regardless of side.
  ///
  /// Mirrors Effect.ts `Either.merge`.
  pub fn merge<A>(e: Either<A, A>) -> A {
    match e {
      Ok(a) | Err(a) => a,
    }
  }

  // ── Conversions ───────────────────────────────────────────────────────────

  /// Convert `Option<R>` to `Either<R, L>`, using `left()` when absent.
  pub fn from_option<R, L>(o: Option<R>, make_left: impl FnOnce() -> L) -> Either<R, L> {
    o.ok_or_else(make_left)
  }

  /// Discard the Left value, returning `Some(r)` for Right and `None` for Left.
  pub fn to_option<R, L>(e: Either<R, L>) -> Option<R> {
    e.ok()
  }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::either;
  use rstest::rstest;

  // ── constructors ─────────────────────────────────────────────────────────

  mod constructors {
    use super::*;

    #[test]
    fn right_creates_ok_variant() {
      assert_eq!(either::right::<i32, &str>(42), Ok(42));
    }

    #[test]
    fn left_creates_err_variant() {
      assert_eq!(either::left::<i32, &str>("fail"), Err("fail"));
    }
  }

  // ── is_right / is_left ────────────────────────────────────────────────────

  mod inspection {
    use super::*;

    #[rstest]
    #[case::right(Ok::<i32, &str>(1), true, false)]
    #[case::left(Err::<i32, &str>("x"), false, true)]
    fn variants(
      #[case] e: Result<i32, &str>,
      #[case] expected_right: bool,
      #[case] expected_left: bool,
    ) {
      assert_eq!(either::is_right(&e), expected_right);
      assert_eq!(either::is_left(&e), expected_left);
    }
  }

  // ── map ───────────────────────────────────────────────────────────────────

  mod map {
    use super::*;

    #[test]
    fn right_value_is_transformed() {
      assert_eq!(either::map(Ok::<i32, &str>(3), |n| n * 2), Ok(6));
    }

    #[test]
    fn left_value_passes_through() {
      assert_eq!(either::map(Err::<i32, &str>("e"), |n| n * 2), Err("e"));
    }
  }

  // ── map_left ──────────────────────────────────────────────────────────────

  mod map_left {
    use super::*;

    #[test]
    fn left_value_is_transformed() {
      assert_eq!(
        either::map_left(Err::<i32, &str>("err"), |s| s.len()),
        Err(3)
      );
    }

    #[test]
    fn right_value_passes_through() {
      assert_eq!(either::map_left(Ok::<i32, &str>(5), |s| s.len()), Ok(5));
    }
  }

  // ── flat_map ─────────────────────────────────────────────────────────────

  mod flat_map {
    use super::*;

    #[test]
    fn right_to_right() {
      assert_eq!(
        either::flat_map(Ok::<i32, &str>(4), |n| Ok::<i32, &str>(n + 1)),
        Ok(5)
      );
    }

    #[test]
    fn right_to_left() {
      assert_eq!(
        either::flat_map(Ok::<i32, &str>(0), |_| Err::<i32, &str>("zero")),
        Err("zero")
      );
    }

    #[test]
    fn left_stays_left() {
      assert_eq!(
        either::flat_map(Err::<i32, &str>("fail"), |n| Ok::<i32, &str>(n + 1)),
        Err("fail")
      );
    }
  }

  // ── flat_map_left ────────────────────────────────────────────────────────

  mod flat_map_left {
    use super::*;

    #[test]
    fn left_to_right_recovery() {
      let e: Result<i32, &str> = Err("retry");
      assert_eq!(either::flat_map_left(e, |_| Ok::<i32, usize>(99)), Ok(99));
    }

    #[test]
    fn left_to_left() {
      let e: Result<i32, &str> = Err("a");
      assert_eq!(
        either::flat_map_left(e, |s| Err::<i32, usize>(s.len())),
        Err(1_usize)
      );
    }

    #[test]
    fn right_passes_through() {
      let e: Result<i32, &str> = Ok(7);
      assert_eq!(either::flat_map_left(e, |_| Ok::<i32, usize>(99)), Ok(7));
    }
  }

  // ── flip ─────────────────────────────────────────────────────────────────

  mod flip {
    use super::*;

    #[test]
    fn right_becomes_left() {
      assert_eq!(either::flip(Ok::<i32, &str>(5)), Err(5));
    }

    #[test]
    fn left_becomes_right() {
      assert_eq!(either::flip(Err::<i32, &str>("x")), Ok("x"));
    }

    #[test]
    fn flip_twice_is_identity() {
      let e: Result<i32, &str> = Ok(42);
      assert_eq!(either::flip(either::flip(e)), e);
    }
  }

  // ── get_or_else ───────────────────────────────────────────────────────────

  mod get_or_else {
    use super::*;

    #[test]
    fn right_returns_inner_value() {
      assert_eq!(either::get_or_else(Ok::<i32, i32>(3), |l| l * 10), 3);
    }

    #[test]
    fn left_applies_default_function() {
      assert_eq!(either::get_or_else(Err::<i32, i32>(5), |l| l * 10), 50);
    }
  }

  // ── or_else ───────────────────────────────────────────────────────────────

  mod or_else {
    use super::*;

    #[test]
    fn right_passes_through() {
      let e: Result<i32, &str> = Ok(1);
      assert_eq!(either::or_else(e, |_| Ok::<i32, usize>(99)), Ok(1));
    }

    #[test]
    fn left_tries_alternative_right() {
      let e: Result<i32, &str> = Err("try again");
      assert_eq!(either::or_else(e, |_| Ok::<i32, usize>(99)), Ok(99));
    }

    #[test]
    fn left_tries_alternative_left() {
      let e: Result<i32, &str> = Err("a");
      assert_eq!(
        either::or_else(e, |s| Err::<i32, usize>(s.len())),
        Err(1_usize)
      );
    }
  }

  // ── merge ─────────────────────────────────────────────────────────────────

  mod merge {
    use super::*;

    #[test]
    fn right_returns_inner() {
      assert_eq!(either::merge(Ok::<i32, i32>(5)), 5);
    }

    #[test]
    fn left_returns_inner() {
      assert_eq!(either::merge(Err::<i32, i32>(7)), 7);
    }
  }

  // ── from_option / to_option ───────────────────────────────────────────────

  mod conversions {
    use super::*;

    #[test]
    fn from_option_some_gives_right() {
      assert_eq!(either::from_option(Some(10_i32), || "missing"), Ok(10));
    }

    #[test]
    fn from_option_none_gives_left() {
      assert_eq!(
        either::from_option(None::<i32>, || "missing"),
        Err("missing")
      );
    }

    #[test]
    fn to_option_right_gives_some() {
      assert_eq!(either::to_option(Ok::<i32, &str>(3)), Some(3));
    }

    #[test]
    fn to_option_left_gives_none() {
      assert_eq!(either::to_option(Err::<i32, &str>("e")), None);
    }
  }
}
