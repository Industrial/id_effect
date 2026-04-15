//! Structural equality and hashing protocols — mirrors Effect.ts `Equal` and `Hash`.
//!
//! Both traits are blanket-implemented for all types that already implement the
//! corresponding `std` trait, so existing code gains the Effect.ts API for free.

use std::hash::Hasher;

// ── Equal ────────────────────────────────────────────────────────────────────

/// Structural equality — Effect.ts `Equal.equals`.
///
/// Blanket-implemented for every `PartialEq` type; override only when you need
/// custom structural semantics.
pub trait Equal: PartialEq {
  /// Returns whether `self` and `other` are structurally equal (defaults to `PartialEq::eq`).
  fn effect_equals(&self, other: &Self) -> bool {
    self == other
  }
}

impl<T: PartialEq + ?Sized> Equal for T {}

/// `Equal.equals` free function.
pub fn equals<A: Equal + ?Sized>(a: &A, b: &A) -> bool {
  a.effect_equals(b)
}

// ── EffectHash ────────────────────────────────────────────────────────────────

/// Structural hash — Effect.ts `Hash.hash`.
///
/// Blanket-implemented for every `std::hash::Hash` type.
pub trait EffectHash: std::hash::Hash {
  /// Returns a `u64` hash of `self` using the default hasher (Effect.ts-style single-value hash).
  fn effect_hash(&self) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(self, &mut h);
    h.finish()
  }
}

impl<T: std::hash::Hash + ?Sized> EffectHash for T {}

/// `Hash.hash` — hash a single value.
pub fn hash<A: EffectHash + ?Sized>(value: &A) -> u64 {
  value.effect_hash()
}

/// `Hash.combine` — combine two hashes.
pub fn combine(h1: u64, h2: u64) -> u64 {
  h1.wrapping_mul(31).wrapping_add(h2)
}

/// `Hash.string` — hash a string slice.
pub fn hash_string(s: &str) -> u64 {
  hash(s)
}

/// `Hash.structure` — hash any `Hash` value (alias of `hash`).
pub fn hash_structure<A: std::hash::Hash + ?Sized>(value: &A) -> u64 {
  hash(value)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  // ── equals ───────────────────────────────────────────────────────────────

  mod equals_fn {
    use super::*;

    #[test]
    fn equal_integers_returns_true() {
      assert!(equals(&42u32, &42u32));
    }

    #[test]
    fn unequal_integers_returns_false() {
      assert!(!equals(&1u32, &2u32));
    }

    #[test]
    fn equal_strings_returns_true() {
      assert!(equals(&"hello".to_string(), &"hello".to_string()));
    }

    #[test]
    fn unequal_strings_returns_false() {
      assert!(!equals(&"a".to_string(), &"b".to_string()));
    }

    #[test]
    fn equal_unit_returns_true() {
      assert!(equals(&(), &()));
    }

    #[rstest]
    #[case::zero(0u32, 0u32, true)]
    #[case::same_positive(100u32, 100u32, true)]
    #[case::different(1u32, 2u32, false)]
    #[case::max(u32::MAX, u32::MAX, true)]
    fn parametrised_integer_cases(#[case] a: u32, #[case] b: u32, #[case] expected: bool) {
      assert_eq!(equals(&a, &b), expected);
    }
  }

  // ── Equal trait ──────────────────────────────────────────────────────────

  mod effect_equals_method {
    use super::*;

    #[test]
    fn method_agrees_with_free_function_for_equal_values() {
      let a = 7u64;
      let b = 7u64;
      assert_eq!(a.effect_equals(&b), equals(&a, &b));
    }

    #[test]
    fn method_agrees_with_free_function_for_unequal_values() {
      let a = 3u64;
      let b = 5u64;
      assert_eq!(a.effect_equals(&b), equals(&a, &b));
    }

    #[test]
    fn method_is_symmetric() {
      let a = "x".to_string();
      let b = "x".to_string();
      assert_eq!(a.effect_equals(&b), b.effect_equals(&a));
    }
  }

  // ── hash ─────────────────────────────────────────────────────────────────

  mod hash_fn {
    use super::*;

    #[test]
    fn same_value_produces_same_hash() {
      assert_eq!(hash(&42u32), hash(&42u32));
    }

    #[test]
    fn different_values_typically_produce_different_hashes() {
      // Not guaranteed, but true for these simple inputs
      assert_ne!(hash(&1u32), hash(&2u32));
    }

    #[test]
    fn hash_returns_deterministic_result() {
      let v = "deterministic".to_string();
      assert_eq!(hash(&v), hash(&v));
    }
  }

  // ── EffectHash trait ─────────────────────────────────────────────────────

  mod effect_hash_method {
    use super::*;

    #[test]
    fn method_agrees_with_free_function() {
      let v = 99u32;
      assert_eq!(v.effect_hash(), hash(&v));
    }

    #[test]
    fn hash_of_zero_is_deterministic() {
      assert_eq!(0u32.effect_hash(), 0u32.effect_hash());
    }
  }

  // ── combine ──────────────────────────────────────────────────────────────

  mod combine_fn {
    use super::*;

    #[test]
    fn combine_two_zeros_returns_zero() {
      assert_eq!(combine(0, 0), 0);
    }

    #[test]
    fn combine_is_not_commutative() {
      // combine(a, b) ≠ combine(b, a) in general
      let ab = combine(1, 2);
      let ba = combine(2, 1);
      assert_ne!(ab, ba);
    }

    #[test]
    fn combine_produces_deterministic_result() {
      assert_eq!(combine(100, 200), combine(100, 200));
    }

    #[rstest]
    #[case::both_max(u64::MAX, u64::MAX)]
    #[case::first_zero(0, 42)]
    #[case::second_zero(42, 0)]
    fn combine_does_not_panic_on_extremes(#[case] h1: u64, #[case] h2: u64) {
      let _ = combine(h1, h2);
    }
  }

  // ── hash_string ──────────────────────────────────────────────────────────

  mod hash_string_fn {
    use super::*;

    #[test]
    fn same_string_produces_same_hash() {
      assert_eq!(hash_string("hello"), hash_string("hello"));
    }

    #[test]
    fn different_strings_produce_different_hashes() {
      assert_ne!(hash_string("foo"), hash_string("bar"));
    }

    #[test]
    fn empty_string_hashes_without_panic() {
      let _ = hash_string("");
    }

    #[test]
    fn hash_string_agrees_with_hash_fn_on_string_slice() {
      let s = "test";
      assert_eq!(hash_string(s), hash(&s));
    }
  }

  // ── hash_structure ───────────────────────────────────────────────────────

  mod hash_structure_fn {
    use super::*;

    #[test]
    fn hash_structure_agrees_with_hash_for_integers() {
      assert_eq!(hash_structure(&42u64), hash(&42u64));
    }

    #[test]
    fn hash_structure_is_deterministic() {
      let v = (1u32, "key".to_string());
      assert_eq!(hash_structure(&v), hash_structure(&v));
    }
  }
}
