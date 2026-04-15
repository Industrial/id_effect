//! **Coproduct** — the categorical coproduct (sum) of types.
//!
//! The coproduct of two types `A` and `B` is a type `Either<A, B>` together with
//! injection functions `left` and `right`. In category theory, coproducts satisfy
//! a universal property: for any type `C` with functions `f: A → C` and `g: B → C`,
//! there exists a unique function `h: Either<A, B> → C` such that `h ∘ left = f`
//! and `h ∘ right = g`.
//!
//! ## Representation
//!
//! We use `Result<R, L>` as the underlying type where:
//! - `Ok(r)` represents `Right(r)` — the "success" or "right" variant
//! - `Err(l)` represents `Left(l)` — the "failure" or "left" variant
//!
//! This aligns with Effect.ts conventions where `Right` is the "happy path".
//!
//! ## Properties
//!
//! - **Injections**: `left(a)` and `right(b)` inject into the coproduct
//! - **Elimination**: `either(f, g)(e)` eliminates the coproduct
//! - **Commutativity**: `Either<A, B> ≅ Either<B, A>` via [`flip`]
//! - **Associativity**: `Either<Either<A, B>, C> ≅ Either<A, Either<B, C>>`
//! - **Never identity**: `Either<Never, A> ≅ A ≅ Either<A, Never>`
//!
//! ## Combinators
//!
//! | Function | Signature | Description |
//! |----------|-----------|-------------|
//! | [`left`] | `L → Either<R, L>` | Left injection |
//! | [`right`] | `R → Either<R, L>` | Right injection |
//! | [`either`] | `(L → C) → (R → C) → (Either<R, L> → C)` | Elimination/case analysis |
//! | [`bimap`] | `(L → L2) → (R → R2) → (Either<R, L> → Either<R2, L2>)` | Map both sides |
//! | [`flip`] | `Either<R, L> → Either<L, R>` | Swap variants |

/// The coproduct type: `Ok(R)` is Right, `Err(L)` is Left.
///
/// This is a type alias over `std::result::Result<R, L>`, providing
/// semantic clarity when used as a pure sum type rather than error handling.
pub type Either<R, L> = Result<R, L>;

/// Inject a value into the Left variant.
///
/// This is the first injection morphism of the categorical coproduct.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::left;
///
/// let e: Result<i32, &str> = left("error");
/// assert_eq!(e, Err("error"));
/// ```
#[inline]
pub fn left<R, L>(l: L) -> Either<R, L> {
  Err(l)
}

/// Inject a value into the Right variant.
///
/// This is the second injection morphism of the categorical coproduct.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::right;
///
/// let e: Result<i32, &str> = right(42);
/// assert_eq!(e, Ok(42));
/// ```
#[inline]
pub fn right<R, L>(r: R) -> Either<R, L> {
  Ok(r)
}

/// Check if an Either is the Left variant.
#[inline]
pub fn is_left<R, L>(e: &Either<R, L>) -> bool {
  e.is_err()
}

/// Check if an Either is the Right variant.
#[inline]
pub fn is_right<R, L>(e: &Either<R, L>) -> bool {
  e.is_ok()
}

/// Eliminate an Either by providing handlers for both cases.
///
/// This is the universal property of coproducts: given `f: L → C` and `g: R → C`,
/// we get a unique `h: Either<R, L> → C`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{either, left, right};
///
/// let handle_left = |s: &str| s.len();
/// let handle_right = |n: i32| n as usize;
///
/// assert_eq!(either(handle_left, handle_right)(left("hello")), 5);
/// assert_eq!(either(handle_left, handle_right)(right(42)), 42);
/// ```
#[inline]
pub fn either<R, L, C>(
  on_left: impl FnOnce(L) -> C,
  on_right: impl FnOnce(R) -> C,
) -> impl FnOnce(Either<R, L>) -> C {
  move |e| match e {
    Ok(r) => on_right(r),
    Err(l) => on_left(l),
  }
}

/// Map the Right value, leaving Left unchanged.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{map, right, left};
///
/// assert_eq!(map(right::<i32, &str>(3), |n| n * 2), Ok(6));
/// assert_eq!(map(left::<i32, &str>("e"), |n| n * 2), Err("e"));
/// ```
#[inline]
pub fn map<R, R2, L>(e: Either<R, L>, f: impl FnOnce(R) -> R2) -> Either<R2, L> {
  e.map(f)
}

/// Map the Left value, leaving Right unchanged.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{map_left, right, left};
///
/// assert_eq!(map_left(left::<i32, &str>("err"), |s| s.len()), Err(3));
/// assert_eq!(map_left(right::<i32, &str>(5), |s| s.len()), Ok(5));
/// ```
#[inline]
pub fn map_left<R, L, L2>(e: Either<R, L>, f: impl FnOnce(L) -> L2) -> Either<R, L2> {
  e.map_err(f)
}

/// Map both sides of an Either independently.
///
/// `bimap(f, g)(e)` applies `f` to Left and `g` to Right.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{bimap, left, right};
///
/// let transform = |e| bimap(e, |s: &str| s.len(), |n: i32| n * 2);
///
/// assert_eq!(transform(left("hello")), Err(5));
/// assert_eq!(transform(right(3)), Ok(6));
/// ```
#[inline]
pub fn bimap<R, R2, L, L2>(
  e: Either<R, L>,
  on_left: impl FnOnce(L) -> L2,
  on_right: impl FnOnce(R) -> R2,
) -> Either<R2, L2> {
  match e {
    Ok(r) => Ok(on_right(r)),
    Err(l) => Err(on_left(l)),
  }
}

/// Flat-map over the Right value.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{flat_map, right, left};
///
/// let half_if_even = |n: i32| {
///     if n % 2 == 0 { Ok(n / 2) } else { Err("odd") }
/// };
///
/// assert_eq!(flat_map(right::<i32, &str>(4), half_if_even), Ok(2));
/// assert_eq!(flat_map(right::<i32, &str>(3), half_if_even), Err("odd"));
/// assert_eq!(flat_map(left::<i32, &str>("e"), half_if_even), Err("e"));
/// ```
#[inline]
pub fn flat_map<R, R2, L>(e: Either<R, L>, f: impl FnOnce(R) -> Either<R2, L>) -> Either<R2, L> {
  e.and_then(f)
}

/// Flat-map over the Left value.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{flat_map_left, right, left};
///
/// let retry = |_: &str| Ok::<i32, usize>(99);
///
/// assert_eq!(flat_map_left(left::<i32, &str>("err"), retry), Ok(99));
/// assert_eq!(flat_map_left(right::<i32, &str>(5), retry), Ok(5));
/// ```
#[inline]
pub fn flat_map_left<R, L, L2>(
  e: Either<R, L>,
  f: impl FnOnce(L) -> Either<R, L2>,
) -> Either<R, L2> {
  match e {
    Ok(r) => Ok(r),
    Err(l) => f(l),
  }
}

/// Swap Left and Right variants.
///
/// This witnesses the commutativity of coproducts: `Either<R, L> ≅ Either<L, R>`.
///
/// # Laws
///
/// - **Involution**: `flip(flip(e)) = e`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{flip, left, right};
///
/// assert_eq!(flip(right::<i32, &str>(5)), Err(5));
/// assert_eq!(flip(left::<i32, &str>("x")), Ok("x"));
/// ```
#[inline]
pub fn flip<R, L>(e: Either<R, L>) -> Either<L, R> {
  match e {
    Ok(r) => Err(r),
    Err(l) => Ok(l),
  }
}

/// Get the Right value or compute a default from the Left.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{get_or_else, right, left};
///
/// assert_eq!(get_or_else(right::<i32, i32>(3), |l| l * 10), 3);
/// assert_eq!(get_or_else(left::<i32, i32>(5), |l| l * 10), 50);
/// ```
#[inline]
pub fn get_or_else<R, L>(e: Either<R, L>, default: impl FnOnce(L) -> R) -> R {
  e.unwrap_or_else(default)
}

/// Try an alternative if Left, keeping Right unchanged.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{or_else, right, left};
///
/// let retry = |_: &str| Ok::<i32, usize>(99);
///
/// assert_eq!(or_else(right::<i32, &str>(1), retry), Ok(1));
/// assert_eq!(or_else(left::<i32, &str>("err"), retry), Ok(99));
/// ```
#[inline]
pub fn or_else<R, L, L2>(e: Either<R, L>, f: impl FnOnce(L) -> Either<R, L2>) -> Either<R, L2> {
  match e {
    Ok(r) => Ok(r),
    Err(l) => f(l),
  }
}

/// Merge both sides when they have the same type.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{merge, left, right};
///
/// assert_eq!(merge(right::<i32, i32>(5)), 5);
/// assert_eq!(merge(left::<i32, i32>(7)), 7);
/// ```
#[inline]
pub fn merge<A>(e: Either<A, A>) -> A {
  match e {
    Ok(a) | Err(a) => a,
  }
}

/// Convert an Option to Either, using a default Left value when None.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::from_option;
///
/// assert_eq!(from_option(Some(10), || "missing"), Ok(10));
/// assert_eq!(from_option(None::<i32>, || "missing"), Err("missing"));
/// ```
#[inline]
pub fn from_option<R, L>(o: Option<R>, make_left: impl FnOnce() -> L) -> Either<R, L> {
  o.ok_or_else(make_left)
}

/// Convert Either to Option, discarding the Left value.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::coproduct::{to_option, right, left};
///
/// assert_eq!(to_option(right::<i32, &str>(3)), Some(3));
/// assert_eq!(to_option(left::<i32, &str>("e")), None);
/// ```
#[inline]
pub fn to_option<R, L>(e: Either<R, L>) -> Option<R> {
  e.ok()
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn left_creates_err_variant() {
      let e: Either<i32, &str> = left("fail");
      assert_eq!(e, Err("fail"));
    }

    #[test]
    fn right_creates_ok_variant() {
      let e: Either<i32, &str> = right(42);
      assert_eq!(e, Ok(42));
    }
  }

  mod inspection {
    use super::*;

    #[rstest]
    #[case::right(Ok::<i32, &str>(1), true, false)]
    #[case::left(Err::<i32, &str>("x"), false, true)]
    fn is_left_and_is_right(
      #[case] e: Either<i32, &str>,
      #[case] expected_right: bool,
      #[case] expected_left: bool,
    ) {
      assert_eq!(is_right(&e), expected_right);
      assert_eq!(is_left(&e), expected_left);
    }
  }

  mod either_elimination {
    use super::*;

    #[test]
    fn either_applies_left_handler() {
      let handler = either(|s: &str| s.len(), |n: i32| n as usize);
      assert_eq!(handler(left("hello")), 5);
    }

    #[test]
    fn either_applies_right_handler() {
      let handler = either(|s: &str| s.len(), |n: i32| n as usize);
      assert_eq!(handler(right(42)), 42);
    }
  }

  mod map_tests {
    use super::*;

    #[test]
    fn map_transforms_right() {
      assert_eq!(map(right::<i32, &str>(3), |n| n * 2), Ok(6));
    }

    #[test]
    fn map_preserves_left() {
      assert_eq!(map(left::<i32, &str>("e"), |n| n * 2), Err("e"));
    }
  }

  mod map_left_tests {
    use super::*;

    #[test]
    fn map_left_transforms_left() {
      assert_eq!(map_left(left::<i32, &str>("err"), |s| s.len()), Err(3));
    }

    #[test]
    fn map_left_preserves_right() {
      assert_eq!(map_left(right::<i32, &str>(5), |s| s.len()), Ok(5));
    }
  }

  mod bimap_tests {
    use super::*;

    #[test]
    fn bimap_transforms_left() {
      let result = bimap(left::<i32, &str>("hello"), |s| s.len(), |n| n * 2);
      assert_eq!(result, Err(5));
    }

    #[test]
    fn bimap_transforms_right() {
      let result = bimap(right::<i32, &str>(3), |s| s.len(), |n| n * 2);
      assert_eq!(result, Ok(6));
    }
  }

  mod flat_map_tests {
    use super::*;

    #[test]
    fn flat_map_right_to_right() {
      assert_eq!(
        flat_map(right::<i32, &str>(4), |n| Ok::<i32, &str>(n + 1)),
        Ok(5)
      );
    }

    #[test]
    fn flat_map_right_to_left() {
      assert_eq!(
        flat_map(right::<i32, &str>(0), |_| Err::<i32, &str>("zero")),
        Err("zero")
      );
    }

    #[test]
    fn flat_map_left_unchanged() {
      assert_eq!(
        flat_map(left::<i32, &str>("fail"), |n| Ok::<i32, &str>(n + 1)),
        Err("fail")
      );
    }
  }

  mod flat_map_left_tests {
    use super::*;

    #[test]
    fn flat_map_left_recovers() {
      let e: Either<i32, &str> = left("retry");
      assert_eq!(flat_map_left(e, |_| Ok::<i32, usize>(99)), Ok(99));
    }

    #[test]
    fn flat_map_left_to_left() {
      let e: Either<i32, &str> = left("a");
      assert_eq!(flat_map_left(e, |s| Err::<i32, usize>(s.len())), Err(1));
    }

    #[test]
    fn flat_map_left_preserves_right() {
      let e: Either<i32, &str> = right(7);
      assert_eq!(flat_map_left(e, |_| Ok::<i32, usize>(99)), Ok(7));
    }
  }

  mod flip_tests {
    use super::*;

    #[test]
    fn flip_right_becomes_left() {
      assert_eq!(flip(right::<i32, &str>(5)), Err(5));
    }

    #[test]
    fn flip_left_becomes_right() {
      assert_eq!(flip(left::<i32, &str>("x")), Ok("x"));
    }

    #[test]
    fn flip_involution() {
      let e: Either<i32, &str> = right(42);
      assert_eq!(flip(flip(e.clone())), e);
    }
  }

  mod get_or_else_tests {
    use super::*;

    #[test]
    fn get_or_else_returns_right() {
      assert_eq!(get_or_else(right::<i32, i32>(3), |l| l * 10), 3);
    }

    #[test]
    fn get_or_else_computes_from_left() {
      assert_eq!(get_or_else(left::<i32, i32>(5), |l| l * 10), 50);
    }
  }

  mod or_else_tests {
    use super::*;

    #[test]
    fn or_else_preserves_right() {
      let e: Either<i32, &str> = right(1);
      assert_eq!(or_else(e, |_| Ok::<i32, usize>(99)), Ok(1));
    }

    #[test]
    fn or_else_tries_alternative() {
      let e: Either<i32, &str> = left("try again");
      assert_eq!(or_else(e, |_| Ok::<i32, usize>(99)), Ok(99));
    }
  }

  mod merge_tests {
    use super::*;

    #[test]
    fn merge_extracts_right() {
      assert_eq!(merge(right::<i32, i32>(5)), 5);
    }

    #[test]
    fn merge_extracts_left() {
      assert_eq!(merge(left::<i32, i32>(7)), 7);
    }
  }

  mod conversions {
    use super::*;

    #[test]
    fn from_option_some_gives_right() {
      assert_eq!(from_option(Some(10_i32), || "missing"), Ok(10));
    }

    #[test]
    fn from_option_none_gives_left() {
      assert_eq!(from_option(None::<i32>, || "missing"), Err("missing"));
    }

    #[test]
    fn to_option_right_gives_some() {
      assert_eq!(to_option(right::<i32, &str>(3)), Some(3));
    }

    #[test]
    fn to_option_left_gives_none() {
      assert_eq!(to_option(left::<i32, &str>("e")), None);
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn either_after_left_injection() {
      // either(f, g) ∘ left = f
      let f = |s: &str| s.len();
      let g = |n: i32| n as usize;

      for s in ["", "a", "hello", "test"] {
        assert_eq!(either(f, g)(left(s)), f(s));
      }
    }

    #[test]
    fn either_after_right_injection() {
      // either(f, g) ∘ right = g
      let f = |s: &str| s.len();
      let g = |n: i32| n as usize;

      for n in [0, 1, 42, 100] {
        assert_eq!(either(f, g)(right(n)), g(n));
      }
    }

    #[test]
    fn map_identity_is_identity() {
      let e: Either<i32, &str> = right(5);
      assert_eq!(map(e.clone(), |x| x), e);
    }

    #[test]
    fn map_composition() {
      // map(e, g ∘ f) = map(map(e, f), g)
      let f = |n: i32| n + 1;
      let g = |n: i32| n * 2;

      for x in [0, 5, -3] {
        let e = right::<i32, &str>(x);
        let left_side = map(e.clone(), |n| g(f(n)));
        let right_side = map(map(e, f), g);
        assert_eq!(left_side, right_side);
      }
    }

    #[test]
    fn flip_is_involution() {
      for e in [right::<i32, &str>(5), left::<i32, &str>("err")] {
        assert_eq!(flip(flip(e.clone())), e);
      }
    }
  }
}
