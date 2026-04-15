//! **Never** — the initial object in the category of types.
//!
//! The never type is the unique type with **zero** inhabitants. In category theory,
//! it is the **initial object**: for every type `A`, there exists exactly one function
//! `Never → A` (the `absurd` function, which is vacuously total).
//!
//! ## Properties
//!
//! - **Cardinality**: `|Never| = 0`
//! - **Initial**: `∀A. ∃! f: Never → A`
//! - **Identity for coproduct**: `Either<Never, A> ≅ A ≅ Either<A, Never>`
//!
//! ## Rust Representation
//!
//! Rust's `!` (never) type is unstable, so we use `core::convert::Infallible` as
//! the stable equivalent. Both are uninhabited types with the same categorical properties.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use id_effect::foundation::never::{absurd, Never};
//!
//! fn handle_impossible(n: Never) -> i32 {
//!     // This function can return any type because it can never be called
//!     absurd(n)
//! }
//! ```

/// The uninhabited/never type — has zero values.
///
/// This is a type alias for `core::convert::Infallible`, Rust's stable
/// representation of the never type `!`.
pub type Never = core::convert::Infallible;

/// Eliminate the never type by producing any value.
///
/// Since `Never` has no inhabitants, this function can never actually be called.
/// It exists to satisfy the type system when handling impossible cases.
///
/// This is the unique morphism from the initial object: `absurd : 0 → A`
#[inline]
pub fn absurd<A>(never: core::convert::Infallible) -> A {
  match never {}
}

#[cfg(test)]
mod tests {
  use super::*;

  mod never_type {
    use super::*;

    #[test]
    fn never_is_zero_sized() {
      // Uninhabited types are zero-sized
      assert_eq!(std::mem::size_of::<Never>(), 0);
    }

    #[test]
    fn never_has_no_alignment_requirements() {
      // Uninhabited types have alignment 1 (minimal)
      assert_eq!(std::mem::align_of::<Never>(), 1);
    }
  }

  mod absurd_function {
    use super::*;

    #[test]
    fn absurd_signature_compiles() {
      // We cannot test absurd at runtime (no Never values exist),
      // but we verify the signature is correct
      fn _uses_absurd(n: Never) -> i32 {
        absurd(n)
      }

      fn _uses_absurd_string(n: Never) -> String {
        absurd(n)
      }
    }

    #[test]
    fn absurd_in_match_arm() {
      // Common pattern: handling Result<T, Never>
      let result: Result<i32, Never> = Ok(42);
      let value = match result {
        Ok(v) => v,
        Err(never) => absurd(never),
      };
      assert_eq!(value, 42);
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn never_is_identity_for_either_left() {
      // Either<Never, A> ≅ A
      // We can convert Either<Never, i32> to i32 without losing information
      let either: Result<i32, Never> = Ok(42);
      let value: i32 = match either {
        Ok(a) => a,
        Err(n) => absurd(n),
      };
      assert_eq!(value, 42);
    }

    #[test]
    fn result_with_never_error_always_succeeds() {
      fn infallible_add(a: i32, b: i32) -> Result<i32, Never> {
        Ok(a + b)
      }

      // We can safely unwrap because failure is impossible
      let result = infallible_add(2, 3);
      assert!(result.is_ok());

      // Pattern: convert Result<T, Never> to T
      let value = match result {
        Ok(v) => v,
        Err(n) => absurd(n),
      };
      assert_eq!(value, 5);
    }
  }

  mod practical_usage {
    use super::*;
    use std::convert::Infallible;

    #[test]
    fn never_is_same_as_infallible() {
      fn _takes_infallible(_: Infallible) {}
      fn _takes_never(_: Never) {}

      // These are the same type
      fn _coerce(n: Never) -> Infallible {
        n
      }
      fn _coerce_back(i: Infallible) -> Never {
        i
      }
    }
  }
}
