//! **Isomorphism** — invertible mappings between types.
//!
// The complex return types on isomorphism functions are intentional: they
// encode the full bijection structure at the type level for maximum clarity.
#![allow(clippy::type_complexity)]
//!
//! An isomorphism between types `A` and `B` is a pair of functions `to: A → B` and
//! `from: B → A` such that they are mutual inverses:
//!
//! - `from(to(a)) = a` for all `a: A`
//! - `to(from(b)) = b` for all `b: B`
//!
//! Isomorphisms witness that two types are "essentially the same" — they contain
//! the same information, just represented differently.
//!
//! ## Properties
//!
//! - **Reflexivity**: Every type is isomorphic to itself (`id, id`)
//! - **Symmetry**: If `A ≅ B`, then `B ≅ A` (swap `to` and `from`)
//! - **Transitivity**: If `A ≅ B` and `B ≅ C`, then `A ≅ C` (compose)
//!
//! ## Examples of Isomorphisms
//!
//! - `(A, B) ≅ (B, A)` via `swap`
//! - `(A, ()) ≅ A` via `fst` / `(a, ())`
//! - `Either<Never, A> ≅ A` via pattern matching
//! - `A ≅ A` via `identity`

/// An isomorphism between types `A` and `B`.
///
/// This struct captures a bijection: two functions that are mutual inverses.
///
/// # Laws
///
/// For a valid isomorphism:
/// - `iso.from(iso.to(a)) == a` for all `a: A`
/// - `iso.to(iso.from(b)) == b` for all `b: B`
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::Iso;
///
/// // Isomorphism between (i32, i32) and (i32, i32) via swap
/// let swap_iso = Iso::new(|(a, b)| (b, a), |(a, b)| (b, a));
///
/// assert_eq!((swap_iso.to)((1, 2)), (2, 1));
/// assert_eq!((swap_iso.from)((2, 1)), (1, 2));
/// ```
#[derive(Clone, Copy)]
pub struct Iso<A, B, To = fn(A) -> B, From = fn(B) -> A> {
  /// Forward direction: `A → B`
  pub to: To,
  /// Backward direction: `B → A`
  pub from: From,
  _marker: std::marker::PhantomData<fn(A) -> B>,
}

impl<A, B, To, From> Iso<A, B, To, From>
where
  To: Fn(A) -> B,
  From: Fn(B) -> A,
{
  /// Create a new isomorphism from `to` and `from` functions.
  ///
  /// # Safety (logical)
  ///
  /// The caller must ensure that `to` and `from` are mutual inverses.
  /// This is not enforced at compile time.
  pub fn new(to: To, from: From) -> Self {
    Self {
      to,
      from,
      _marker: std::marker::PhantomData,
    }
  }

  /// Apply the forward direction.
  #[inline]
  pub fn forward(&self, a: A) -> B {
    (self.to)(a)
  }

  /// Apply the backward direction.
  #[inline]
  pub fn backward(&self, b: B) -> A {
    (self.from)(b)
  }
}

impl<A, B, To, From> Iso<A, B, To, From>
where
  To: Fn(A) -> B + Clone,
  From: Fn(B) -> A + Clone,
{
  /// Reverse the isomorphism: `Iso<A, B> → Iso<B, A>`.
  ///
  /// This witnesses the symmetry of isomorphism.
  pub fn reverse(self) -> Iso<B, A, From, To> {
    Iso {
      to: self.from,
      from: self.to,
      _marker: std::marker::PhantomData,
    }
  }
}

/// The identity isomorphism: `A ≅ A`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::identity;
///
/// let iso = identity::<i32>();
/// assert_eq!((iso.to)(42), 42);
/// assert_eq!((iso.from)(42), 42);
/// ```
#[inline]
pub fn identity<A>() -> Iso<A, A, fn(A) -> A, fn(A) -> A> {
  Iso {
    to: |a| a,
    from: |a| a,
    _marker: std::marker::PhantomData,
  }
}

/// Swap isomorphism: `(A, B) ≅ (B, A)`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::swap;
///
/// let iso = swap::<i32, &str>();
/// assert_eq!((iso.to)((1, "hello")), ("hello", 1));
/// assert_eq!((iso.from)(("hello", 1)), (1, "hello"));
/// ```
#[inline]
pub fn swap<A, B>() -> Iso<(A, B), (B, A), fn((A, B)) -> (B, A), fn((B, A)) -> (A, B)> {
  Iso {
    to: |(a, b)| (b, a),
    from: |(b, a)| (a, b),
    _marker: std::marker::PhantomData,
  }
}

/// Unit introduction (right): `A ≅ (A, ())`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::unit_right;
///
/// let iso = unit_right::<i32>();
/// assert_eq!((iso.to)(42), (42, ()));
/// assert_eq!((iso.from)((42, ())), 42);
/// ```
#[inline]
pub fn unit_right<A>() -> Iso<A, (A, ()), fn(A) -> (A, ()), fn((A, ())) -> A> {
  Iso {
    to: |a| (a, ()),
    from: |(a, ())| a,
    _marker: std::marker::PhantomData,
  }
}

/// Unit introduction (left): `A ≅ ((), A)`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::unit_left;
///
/// let iso = unit_left::<i32>();
/// assert_eq!((iso.to)(42), ((), 42));
/// assert_eq!((iso.from)(((), 42)), 42);
/// ```
#[inline]
pub fn unit_left<A>() -> Iso<A, ((), A), fn(A) -> ((), A), fn(((), A)) -> A> {
  Iso {
    to: |a| ((), a),
    from: |((), a)| a,
    _marker: std::marker::PhantomData,
  }
}

/// Product associativity: `((A, B), C) ≅ (A, (B, C))`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::assoc_product;
///
/// let iso = assoc_product::<i32, i32, i32>();
/// assert_eq!((iso.to)(((1, 2), 3)), (1, (2, 3)));
/// assert_eq!((iso.from)((1, (2, 3))), ((1, 2), 3));
/// ```
#[inline]
pub fn assoc_product<A, B, C>()
-> Iso<((A, B), C), (A, (B, C)), fn(((A, B), C)) -> (A, (B, C)), fn((A, (B, C))) -> ((A, B), C)> {
  Iso {
    to: |((a, b), c)| (a, (b, c)),
    from: |(a, (b, c))| ((a, b), c),
    _marker: std::marker::PhantomData,
  }
}

/// Uncurry a curried function: convert `A → (B → C)` into `(A, B) → C`.
///
/// # Examples
///
/// ```rust
/// use id_effect::foundation::isomorphism::uncurry;
///
/// let curried_add = |a: i32| move |b: i32| a + b;
/// let uncurried = uncurry(curried_add);
/// assert_eq!(uncurried((2, 3)), 5);
/// ```
#[inline]
pub fn uncurry<A, B, C, F, G>(f: F) -> impl Fn((A, B)) -> C
where
  F: Fn(A) -> G,
  G: Fn(B) -> C,
{
  move |(a, b)| f(a)(b)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod iso_struct {
    use super::*;

    #[test]
    fn iso_new_creates_isomorphism() {
      let iso = Iso::new(|n: i32| n.to_string(), |s: String| s.parse().unwrap_or(0));

      assert_eq!(iso.forward(42), "42");
      assert_eq!(iso.backward("42".to_string()), 42);
    }

    #[test]
    fn iso_reverse_swaps_directions() {
      let iso = Iso::new(|n: i32| n as f64, |f: f64| f as i32);
      let reversed = iso.reverse();

      assert_eq!(reversed.forward(3.7), 3);
      assert_eq!(reversed.backward(5), 5.0);
    }
  }

  mod identity_iso {
    use super::*;

    #[rstest]
    #[case::integer(42_i32)]
    #[case::zero(0_i32)]
    #[case::negative(-7_i32)]
    fn identity_roundtrips(#[case] value: i32) {
      let iso = identity::<i32>();
      assert_eq!((iso.to)(value), value);
      assert_eq!((iso.from)(value), value);
    }

    #[test]
    fn identity_with_string() {
      let iso = identity::<String>();
      let s = String::from("hello");
      assert_eq!((iso.to)(s.clone()), s);
    }
  }

  mod swap_iso {
    use super::*;

    #[test]
    fn swap_exchanges_components() {
      let iso = swap::<i32, &str>();
      assert_eq!((iso.to)((1, "hello")), ("hello", 1));
    }

    #[test]
    fn swap_roundtrips() {
      let iso = swap::<i32, i32>();
      let pair = (1, 2);
      assert_eq!((iso.from)((iso.to)(pair)), pair);
    }

    #[test]
    fn swap_is_self_inverse() {
      let iso = swap::<i32, i32>();
      // swap ∘ swap = id
      let pair = (3, 7);
      assert_eq!((iso.to)((iso.to)(pair)), pair);
    }
  }

  mod unit_isos {
    use super::*;

    #[test]
    fn unit_right_adds_unit() {
      let iso = unit_right::<i32>();
      assert_eq!((iso.to)(42), (42, ()));
    }

    #[test]
    fn unit_right_roundtrips() {
      let iso = unit_right::<i32>();
      assert_eq!((iso.from)((iso.to)(42)), 42);
    }

    #[test]
    fn unit_left_adds_unit() {
      let iso = unit_left::<i32>();
      assert_eq!((iso.to)(42), ((), 42));
    }

    #[test]
    fn unit_left_roundtrips() {
      let iso = unit_left::<i32>();
      assert_eq!((iso.from)((iso.to)(42)), 42);
    }
  }

  mod assoc_product_iso {
    use super::*;

    #[test]
    fn assoc_product_reassociates() {
      let iso = assoc_product::<i32, i32, i32>();
      assert_eq!((iso.to)(((1, 2), 3)), (1, (2, 3)));
    }

    #[test]
    fn assoc_product_roundtrips() {
      let iso = assoc_product::<i32, i32, i32>();
      let left_assoc = ((1, 2), 3);
      assert_eq!((iso.from)((iso.to)(left_assoc)), left_assoc);
    }
  }

  mod uncurry_tests {
    use super::*;

    #[test]
    fn uncurry_converts_curried_function() {
      let curried_add = |a: i32| move |b: i32| a + b;
      let uncurried = uncurry(curried_add);
      assert_eq!(uncurried((2, 3)), 5);
    }

    #[test]
    fn uncurry_with_subtraction() {
      let curried_sub = |a: i32| move |b: i32| a - b;
      let uncurried = uncurry(curried_sub);
      assert_eq!(uncurried((10, 3)), 7);
    }

    #[test]
    fn uncurry_with_multiple_inputs() {
      let curried = |a: i32| move |b: i32| a * b;
      let uncurried = uncurry(curried);

      for (a, b) in [(1, 2), (0, 5), (-3, 4)] {
        assert_eq!(uncurried((a, b)), a * b);
      }
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn identity_is_reflexive() {
      let iso = identity::<i32>();
      for x in [0, 1, -5, 100] {
        assert_eq!((iso.from)((iso.to)(x)), x);
        assert_eq!((iso.to)((iso.from)(x)), x);
      }
    }

    #[test]
    fn swap_is_symmetric() {
      let iso = swap::<i32, i32>();
      let reversed = iso.reverse();

      // Original and reversed should be the same for swap
      for pair in [(1, 2), (0, 0), (-5, 10)] {
        assert_eq!((iso.to)(pair), (reversed.from)(pair));
        assert_eq!((iso.from)(pair), (reversed.to)(pair));
      }
    }

    #[test]
    fn unit_right_inverse_law() {
      let iso = unit_right::<i32>();
      for x in [0, 42, -7] {
        // from(to(x)) = x
        assert_eq!((iso.from)((iso.to)(x)), x);
      }
    }

    #[test]
    fn unit_right_inverse_law_reverse() {
      let iso = unit_right::<i32>();
      for x in [0, 42, -7] {
        // to(from((x, ()))) = (x, ())
        let pair = (x, ());
        assert_eq!((iso.to)((iso.from)(pair)), pair);
      }
    }

    #[test]
    fn assoc_product_inverse_laws() {
      let iso = assoc_product::<i32, i32, i32>();

      let left_assoc = ((1, 2), 3);
      let right_assoc = (1, (2, 3));

      assert_eq!((iso.from)((iso.to)(left_assoc)), left_assoc);
      assert_eq!((iso.to)((iso.from)(right_assoc)), right_assoc);
    }
  }
}
