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

  #[test]
  fn coproduct_api_smoke() {
    assert_eq!(left::<i32, &str>("fail"), Err("fail"));
    assert_eq!(right::<i32, &str>(42), Ok(42));
    assert!(is_right(&Ok::<i32, &str>(1)));
    assert!(is_left(&Err::<i32, &str>("x")));

    assert_eq!(
      either(|s: &str| s.len(), |n: i32| n as usize)(left("hello")),
      5
    );
    assert_eq!(
      either(|s: &str| s.len(), |n: i32| n as usize)(right(42)),
      42
    );

    assert_eq!(map(right::<i32, &str>(3), |n| n * 2), Ok(6));
    assert_eq!(map(left::<i32, &str>("e"), |n: i32| n * 2), Err("e"));
    assert_eq!(map_left(left::<i32, &str>("err"), |s| s.len()), Err(3));
    assert_eq!(map_left(right::<i32, &str>(5), |s: &str| s.len()), Ok(5));

    assert_eq!(
      bimap(left::<i32, &str>("hello"), |s| s.len(), |n: i32| n * 2),
      Err(5)
    );
    assert_eq!(
      bimap(right::<i32, &str>(3), |s: &str| s.len(), |n| n * 2),
      Ok(6)
    );

    assert_eq!(flat_map(right(4), |n| Ok::<i32, &str>(n + 1)), Ok(5));
    assert_eq!(
      flat_map(left::<i32, &str>("fail"), |n: i32| Ok::<i32, &str>(n + 1)),
      Err("fail")
    );
    assert_eq!(
      flat_map_left(left::<i32, &str>("retry"), |_| Ok::<i32, &str>(99)),
      Ok(99)
    );
    assert_eq!(
      flat_map_left(right::<i32, &str>(7), |_| Ok::<i32, &str>(99)),
      Ok(7)
    );

    assert_eq!(flip(right::<i32, &str>(5)), Err(5));
    assert_eq!(flip(left::<i32, &str>("x")), Ok("x"));

    assert_eq!(get_or_else(right::<i32, i32>(3), |l| l * 10), 3);
    assert_eq!(get_or_else(left::<i32, i32>(5), |l| l * 10), 50);
    assert_eq!(
      or_else(left::<i32, &str>("e"), |_| Ok::<i32, &str>(99)),
      Ok(99)
    );
    assert_eq!(merge(right::<i32, i32>(5)), 5);
    assert_eq!(from_option(Some(10_i32), || "missing"), Ok(10));
    assert_eq!(from_option(None::<i32>, || "missing"), Err("missing"));
    assert_eq!(to_option(right::<i32, &str>(3)), Some(3));
    assert_eq!(to_option(left::<i32, &str>("x")), None);
    assert_eq!(
      or_else(right::<i32, &str>(1), |_| Ok::<i32, &str>(99)),
      Ok(1)
    );

    let f = |x: i32| x + 10;
    let g = |x: i32| x * 3;
    let e = right::<i32, &str>(5);
    assert_eq!(map(e.clone(), |x| f(g(x))), map(map(e, g), f));
    assert_eq!(flip(flip(right::<i32, &str>(5))), right(5));
  }
}
