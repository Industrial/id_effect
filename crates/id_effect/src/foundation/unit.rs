//! **Unit** — the terminal object in the category of types.
//!
//! The unit type `()` is the unique type with exactly one inhabitant. In category theory,
//! it is the **terminal object**: for every type `A`, there exists exactly one function
//! `A → ()` (the constant function that discards its input).
//!
//! ## Properties
//!
//! - **Cardinality**: `|()| = 1`
//! - **Terminal**: `∀A. ∃! f: A → ()`
//! - **Identity for product**: `(A, ()) ≅ A ≅ ((), A)`
//!
//! ## Usage
//!
//! ```rust
//! use id_effect::foundation::unit::{discard, unit, Unit};
//!
//! // Unit is the "no information" type
//! let _: Unit = unit();
//!
//! // Any value can be discarded to unit
//! let _: Unit = discard(42);
//! let _: Unit = discard("hello");
//! ```

/// Type alias for the unit type `()`.
///
/// Using `Unit` instead of `()` can improve readability in type signatures
/// where the role of the type is semantic (e.g., "no meaningful value").
pub type Unit = ();

/// Construct the unit value.
///
/// This is the unique inhabitant of the unit type.
#[inline]
pub const fn unit() -> Unit {}

/// Discard any value, returning unit.
///
/// This is the unique morphism from any type to the terminal object.
/// In category theory notation: `!_A : A → 1`
#[inline]
pub fn discard<A>(_: A) -> Unit {}

/// Extend unit to any type by providing a value.
///
/// This witnesses that unit is a "neutral element" — we can always
/// produce a value if we have one ready.
#[inline]
pub fn extend<A>(value: A) -> impl FnOnce(Unit) -> A {
  move |()| value
}

#[cfg(test)]
mod tests {
  use super::*;

  mod unit_value {
    use super::*;

    #[test]
    fn unit_returns_unit_type() {
      let u: Unit = unit();
      assert_eq!(u, ());
    }

    #[test]
    fn unit_is_zero_sized() {
      assert_eq!(std::mem::size_of::<Unit>(), 0);
    }
  }

  mod discard {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::integer(42_i32)]
    #[case::string("hello")]
    #[case::tuple((1, 2, 3))]
    fn discard_returns_unit_for_any_value<T>(#[case] value: T) {
      let result: Unit = discard(value);
      assert_eq!(result, ());
    }

    #[test]
    fn discard_consumes_value() {
      let s = String::from("owned");
      let _: Unit = discard(s);
      // s is moved, cannot use it here
    }
  }

  mod extend {
    use super::*;

    #[test]
    fn extend_produces_value_from_unit() {
      let f = extend(42_i32);
      assert_eq!(f(()), 42);
    }

    #[test]
    fn extend_with_string() {
      let f = extend(String::from("hello"));
      assert_eq!(f(()), "hello");
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn discard_then_extend_recovers_value() {
      // discard loses information, but extend(v) ∘ discard ≡ const v
      let v = 99_i32;
      let f = extend(v);
      let result = f(discard(42_i32));
      assert_eq!(result, v);
    }

    #[test]
    fn unit_is_identity_element_for_product_left() {
      // ((), A) ≅ A witnessed by snd
      let pair: (Unit, i32) = ((), 42);
      assert_eq!(pair.1, 42);
    }

    #[test]
    fn unit_is_identity_element_for_product_right() {
      // (A, ()) ≅ A witnessed by fst
      let pair: (i32, Unit) = (42, ());
      assert_eq!(pair.0, 42);
    }
  }
}
