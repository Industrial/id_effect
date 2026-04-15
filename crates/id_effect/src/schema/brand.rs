//! Branded newtypes — zero-cost compile-time distinction with optional runtime validation.
//!
//! Mirrors Effect.ts `Brand<A, B>` and `Brand.refined`.
//!
//! For the common case, use the [`brand!`](macro@crate::brand) macro to define a named newtype:
//! ```rust,ignore
//! brand!(UserId, u64);  // creates pub struct UserId(pub u64)
//! ```
//!
//! For runtime-validated brands, use [`RefinedBrand`].

use std::marker::PhantomData;

// ── Brand<A, B> ───────────────────────────────────────────────────────────────

/// Zero-cost branded wrapper.
///
/// `Brand<A, B>` wraps a value of type `A` and tags it at the type level with `B`.
/// There is no runtime cost — it compiles to the same layout as `A`.
///
/// Mirrors Effect.ts `Brand.nominal`.
#[repr(transparent)]
pub struct Brand<A, B> {
  inner: A,
  _tag: PhantomData<B>,
}

impl<A, B> Brand<A, B> {
  /// Wrap `value` without runtime validation (`Brand.nominal`).
  pub fn nominal(value: A) -> Brand<A, B> {
    Brand {
      inner: value,
      _tag: PhantomData,
    }
  }

  /// Borrow the inner value.
  pub fn value(&self) -> &A {
    &self.inner
  }

  /// Consume the brand and return the inner value.
  pub fn into_inner(self) -> A {
    self.inner
  }
}

// Standard derives need manual impl because PhantomData<B> may not be Debug/Clone/etc.

impl<A: Clone, B> Clone for Brand<A, B> {
  fn clone(&self) -> Self {
    Brand {
      inner: self.inner.clone(),
      _tag: PhantomData,
    }
  }
}

impl<A: Copy, B> Copy for Brand<A, B> {}

impl<A: std::fmt::Debug, B> std::fmt::Debug for Brand<A, B> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.inner.fmt(f)
  }
}

impl<A: PartialEq, B> PartialEq for Brand<A, B> {
  fn eq(&self, other: &Self) -> bool {
    self.inner == other.inner
  }
}

impl<A: Eq, B> Eq for Brand<A, B> {}

impl<A: std::hash::Hash, B> std::hash::Hash for Brand<A, B> {
  fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
    self.inner.hash(state);
  }
}

impl<A: PartialOrd, B> PartialOrd for Brand<A, B> {
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    self.inner.partial_cmp(&other.inner)
  }
}

impl<A: Ord, B> Ord for Brand<A, B> {
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    self.inner.cmp(&other.inner)
  }
}

impl<A: std::fmt::Display, B> std::fmt::Display for Brand<A, B> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.inner.fmt(f)
  }
}

// ── RefinedBrand<A, B, E> ─────────────────────────────────────────────────────

/// A branded constructor that validates the inner value at runtime.
///
/// Mirrors Effect.ts `Brand.refined(refine, onFailure)`.
#[allow(clippy::type_complexity)]
pub struct RefinedBrand<A, B, E = String> {
  refine: Box<dyn Fn(&A) -> Result<(), E> + Send + Sync>,
  _tag: PhantomData<B>,
}

impl<A: Clone, B, E: Clone> RefinedBrand<A, B, E> {
  /// Create a new refined brand constructor with a validation function.
  ///
  /// `refine` returns `Ok(())` when the value is valid, `Err(e)` otherwise.
  pub fn new(refine: impl Fn(&A) -> Result<(), E> + Send + Sync + 'static) -> Self {
    RefinedBrand {
      refine: Box::new(refine),
      _tag: PhantomData,
    }
  }

  /// Try to brand `value`. Returns `Ok(Brand<A,B>)` or `Err(E)`.
  pub fn try_make(&self, value: A) -> Result<Brand<A, B>, E> {
    (self.refine)(&value)?;
    Ok(Brand::nominal(value))
  }

  /// Returns `Some(Brand<A,B>)` if valid, `None` otherwise.
  pub fn make_option(&self, value: A) -> Option<Brand<A, B>> {
    self.try_make(value).ok()
  }

  /// Returns `true` if `value` would pass validation.
  pub fn is(&self, value: &A) -> bool {
    (self.refine)(value).is_ok()
  }
}

// ── brand! macro ──────────────────────────────────────────────────────────────

/// Define a named branded newtype with standard derives.
///
/// Use `brand!(Name, Inner, omit_display)` for the same shape **without** a derived
/// [`Display`](std::fmt::Display) (when you implement `Display` yourself).
///
/// ```rust,ignore
/// brand!(UserId, u64);
/// // Produces:
/// // #[repr(transparent)]
/// // #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
/// // pub struct UserId(pub u64);
/// // impl UserId { pub fn new(v: u64) -> Self { Self(v) }
/// //               pub fn into_inner(self) -> u64 { self.0 } }
/// ```
#[macro_export]
macro_rules! brand {
  ($name:ident, $inner:ty) => {
    $crate::brand! { @impl $name, $inner, [display] }
  };
  ($name:ident, $inner:ty, omit_display) => {
    $crate::brand! { @impl $name, $inner, [] }
  };
  (@impl $name:ident, $inner:ty, [display]) => {
    #[repr(transparent)]
    #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub struct $name(
      /// Wrapped inner value (unbranded).
      pub $inner,
    );

    impl $name {
      /// Create a new branded value.
      pub fn new(v: $inner) -> Self {
        Self(v)
      }

      /// Unwrap to the inner value.
      pub fn into_inner(self) -> $inner {
        self.0
      }
    }

    impl ::std::fmt::Display for $name
    where
      $inner: ::std::fmt::Display,
    {
      fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        self.0.fmt(f)
      }
    }
  };
  (@impl $name:ident, $inner:ty, []) => {
    #[repr(transparent)]
    #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub struct $name(
      /// Wrapped inner value (unbranded).
      pub $inner,
    );

    impl $name {
      /// Create a new branded value.
      pub fn new(v: $inner) -> Self {
        Self(v)
      }

      /// Unwrap to the inner value.
      pub fn into_inner(self) -> $inner {
        self.0
      }
    }
  };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  // phantom tag types used only in tests
  struct FooTag;
  struct BarTag;

  // ── Brand::nominal ───────────────────────────────────────────────────────

  mod nominal {
    use super::*;

    #[test]
    fn nominal_wraps_integer_and_value_returns_it() {
      let b: Brand<u32, FooTag> = Brand::nominal(42);
      assert_eq!(*b.value(), 42);
    }

    #[test]
    fn nominal_wraps_string_and_into_inner_recovers_it() {
      let b: Brand<String, FooTag> = Brand::nominal("hello".to_string());
      assert_eq!(b.into_inner(), "hello");
    }

    #[test]
    fn nominal_with_zero_value() {
      let b: Brand<u64, FooTag> = Brand::nominal(0);
      assert_eq!(*b.value(), 0);
    }

    #[rstest]
    #[case::one(1u32)]
    #[case::max(u32::MAX)]
    #[case::zero(0u32)]
    fn nominal_preserves_various_integers(#[case] v: u32) {
      let b: Brand<u32, FooTag> = Brand::nominal(v);
      assert_eq!(*b.value(), v);
    }
  }

  // ── Clone / Copy / PartialEq / Ord ───────────────────────────────────────

  mod derives {
    use super::*;

    #[test]
    fn clone_produces_equal_brand() {
      let b: Brand<u32, FooTag> = Brand::nominal(7);
      let c = b;
      assert_eq!(b, c);
    }

    #[test]
    fn copy_semantics_work() {
      let b: Brand<u32, FooTag> = Brand::nominal(5);
      let c = b; // copy
      assert_eq!(*c.value(), 5);
      assert_eq!(*b.value(), 5); // original still usable
    }

    #[test]
    fn equal_inner_values_are_equal_brands() {
      let a: Brand<u32, FooTag> = Brand::nominal(10);
      let b: Brand<u32, FooTag> = Brand::nominal(10);
      assert_eq!(a, b);
    }

    #[test]
    fn different_inner_values_are_unequal_brands() {
      let a: Brand<u32, FooTag> = Brand::nominal(1);
      let b: Brand<u32, FooTag> = Brand::nominal(2);
      assert_ne!(a, b);
    }

    #[test]
    fn ord_follows_inner_value_order() {
      let a: Brand<u32, FooTag> = Brand::nominal(1);
      let b: Brand<u32, FooTag> = Brand::nominal(2);
      assert!(a < b);
      assert!(b > a);
    }

    #[test]
    fn hash_same_value_produces_same_hash() {
      use std::collections::hash_map::DefaultHasher;
      use std::hash::{Hash, Hasher};
      let b: Brand<u32, FooTag> = Brand::nominal(99);
      let mut h1 = DefaultHasher::new();
      let mut h2 = DefaultHasher::new();
      b.hash(&mut h1);
      b.hash(&mut h2);
      assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn different_tag_types_are_different_rust_types() {
      // Compile-time: Brand<u32, FooTag> ≠ Brand<u32, BarTag>
      // This test just confirms both can be instantiated
      let _a: Brand<u32, FooTag> = Brand::nominal(1);
      let _b: Brand<u32, BarTag> = Brand::nominal(1);
    }
  }

  // ── RefinedBrand ─────────────────────────────────────────────────────────

  mod refined_brand {
    use super::*;

    fn positive_u32() -> RefinedBrand<u32, FooTag, String> {
      RefinedBrand::new(|&v| {
        if v > 0 {
          Ok(())
        } else {
          Err("must be positive".to_string())
        }
      })
    }

    #[test]
    fn try_make_with_valid_value_returns_ok() {
      let rb = positive_u32();
      assert!(rb.try_make(5).is_ok());
    }

    #[test]
    fn try_make_with_invalid_value_returns_err() {
      let rb = positive_u32();
      assert!(rb.try_make(0).is_err());
    }

    #[test]
    fn try_make_ok_brand_holds_correct_value() {
      let rb = positive_u32();
      let b = rb.try_make(42).unwrap();
      assert_eq!(*b.value(), 42);
    }

    #[test]
    fn try_make_err_contains_validation_message() {
      let rb = positive_u32();
      let e = rb.try_make(0).unwrap_err();
      assert_eq!(e, "must be positive");
    }

    #[test]
    fn make_option_valid_returns_some() {
      let rb = positive_u32();
      assert!(rb.make_option(10).is_some());
    }

    #[test]
    fn make_option_invalid_returns_none() {
      let rb = positive_u32();
      assert!(rb.make_option(0).is_none());
    }

    #[test]
    fn is_returns_true_for_valid_value() {
      let rb = positive_u32();
      assert!(rb.is(&5));
    }

    #[test]
    fn is_returns_false_for_invalid_value() {
      let rb = positive_u32();
      assert!(!rb.is(&0));
    }

    #[rstest]
    #[case::one(1u32, true)]
    #[case::zero(0u32, false)]
    #[case::max(u32::MAX, true)]
    fn is_parametrised(#[case] v: u32, #[case] expected: bool) {
      let rb = positive_u32();
      assert_eq!(rb.is(&v), expected);
    }
  }

  // ── brand! macro ─────────────────────────────────────────────────────────

  #[allow(dead_code)]
  mod brand_macro {
    // brand! is #[macro_export] so it lives at the crate root — no `use` needed.

    brand!(TestId, u64);
    brand!(TestName, String);

    #[test]
    fn macro_new_creates_value() {
      let id = TestId::new(42);
      assert_eq!(id.0, 42);
    }

    #[test]
    fn macro_into_inner_recovers_value() {
      let id = TestId::new(100);
      assert_eq!(id.into_inner(), 100);
    }

    #[test]
    fn macro_derives_equality() {
      assert_eq!(TestId::new(5), TestId::new(5));
      assert_ne!(TestId::new(1), TestId::new(2));
    }

    #[test]
    fn macro_derives_ordering() {
      assert!(TestId::new(1) < TestId::new(2));
    }

    #[test]
    fn macro_derives_clone() {
      let a = TestId::new(7);
      let b = a.clone();
      assert_eq!(a, b);
    }

    #[test]
    fn macro_works_with_string_inner_type() {
      let n = TestName::new("hello".to_string());
      assert_eq!(n.0, "hello");
    }

    #[test]
    fn macro_display_delegates_to_inner() {
      let id = TestId::new(99);
      assert_eq!(format!("{id}"), "99");
    }
  }
}
