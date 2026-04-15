//! **Semigroup** — a type with an associative binary operation.
//!
//! A semigroup is the simplest algebraic structure: a set with a single binary
//! operation that is associative. No identity element is required.
//!
//! ## Definition
//!
//! ```text
//! SEMIGROUP[A] ::= (A, combine: A → A → A)
//! ```
//!
//! ## Law
//!
//! **Associativity**: `combine(combine(a, b), c) = combine(a, combine(b, c))`
//!
//! ## Examples in this system
//!
//! - `String` with concatenation
//! - `Vec<T>` with concatenation
//! - `Duration` with addition
//! - `Ordering` with "first non-equal wins" (`ordering::combine`)
//! - `Cause<E>` with `Both` (parallel composition)
//! - `Option<A>` where `A: Semigroup` with inner combine
//!
//! ## Relationship to Stratum 0
//!
//! - Uses: [`Functions`](super::super::foundation::function) for composition laws
//! - Used by: [`Monoid`](super::monoid) (semigroup + identity)

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;

/// A type with an associative binary operation.
///
/// # Laws
///
/// Implementations must satisfy:
///
/// ```text
/// combine(combine(a, b), c) = combine(a, combine(b, c))  // Associativity
/// ```
///
/// # Examples
///
/// ```rust
/// use effect_rs::algebra::Semigroup;
///
/// // Strings form a semigroup under concatenation
/// let a = "hello".to_string();
/// let b = " ".to_string();
/// let c = "world".to_string();
///
/// // Associativity: (a ++ b) ++ c == a ++ (b ++ c)
/// assert_eq!(
///     a.clone().combine(b.clone()).combine(c.clone()),
///     a.clone().combine(b.clone().combine(c.clone()))
/// );
/// ```
pub trait Semigroup: Sized {
  /// Combine two values associatively.
  fn combine(self, other: Self) -> Self;

  /// Combine by reference, cloning as needed.
  fn combine_ref(&self, other: &Self) -> Self
  where
    Self: Clone,
  {
    self.clone().combine(other.clone())
  }
}

/// Combine two semigroup values (free function).
#[inline]
pub fn combine<A: Semigroup>(a: A, b: A) -> A {
  a.combine(b)
}

/// Combine all values in an iterator, returning `None` if empty.
///
/// This is `reduce` with the semigroup operation.
pub fn combine_all<A: Semigroup>(iter: impl IntoIterator<Item = A>) -> Option<A> {
  iter.into_iter().reduce(|acc, x| acc.combine(x))
}

/// Combine all values, using a provided first element if the iterator is empty.
pub fn combine_all_or<A: Semigroup>(first: A, iter: impl IntoIterator<Item = A>) -> A {
  iter.into_iter().fold(first, |acc, x| acc.combine(x))
}

/// Repeat a value `n` times using the semigroup operation.
///
/// Returns `None` if `n == 0` (no identity available in a semigroup).
pub fn repeat<A: Semigroup + Clone>(value: A, n: usize) -> Option<A> {
  if n == 0 {
    return None;
  }
  let mut result = value.clone();
  for _ in 1..n {
    result = result.combine(value.clone());
  }
  Some(result)
}

// ── Instances ────────────────────────────────────────────────────────────────

impl Semigroup for String {
  #[inline]
  fn combine(mut self, other: Self) -> Self {
    self.push_str(&other);
    self
  }
}

impl<T> Semigroup for Vec<T> {
  #[inline]
  fn combine(mut self, other: Self) -> Self {
    self.extend(other);
    self
  }
}

impl Semigroup for std::time::Duration {
  #[inline]
  fn combine(self, other: Self) -> Self {
    self.saturating_add(other)
  }
}

impl Semigroup for Ordering {
  /// "First non-equal wins" — return `self` unless it's `Equal`, then `other`.
  #[inline]
  fn combine(self, other: Self) -> Self {
    match self {
      Ordering::Equal => other,
      _ => self,
    }
  }
}

impl<A: Semigroup> Semigroup for Option<A> {
  /// Combine inner values if both are `Some`; otherwise return the `Some`.
  #[inline]
  fn combine(self, other: Self) -> Self {
    match (self, other) {
      (Some(a), Some(b)) => Some(a.combine(b)),
      (Some(a), None) => Some(a),
      (None, Some(b)) => Some(b),
      (None, None) => None,
    }
  }
}

impl<K: Ord, V: Semigroup + Clone> Semigroup for BTreeMap<K, V> {
  /// Merge maps, combining values for duplicate keys.
  fn combine(mut self, other: Self) -> Self {
    for (k, v) in other {
      match self.entry(k) {
        std::collections::btree_map::Entry::Occupied(mut e) => {
          let existing = e.get().clone();
          e.insert(existing.combine(v));
        }
        std::collections::btree_map::Entry::Vacant(e) => {
          e.insert(v);
        }
      }
    }
    self
  }
}

impl<K: Eq + Hash, V: Semigroup + Clone> Semigroup for HashMap<K, V> {
  /// Merge maps, combining values for duplicate keys.
  fn combine(mut self, other: Self) -> Self {
    for (k, v) in other {
      match self.entry(k) {
        std::collections::hash_map::Entry::Occupied(mut e) => {
          let existing = e.get().clone();
          e.insert(existing.combine(v));
        }
        std::collections::hash_map::Entry::Vacant(e) => {
          e.insert(v);
        }
      }
    }
    self
  }
}

impl<T: Ord> Semigroup for BTreeSet<T> {
  /// Set union.
  #[inline]
  fn combine(mut self, other: Self) -> Self {
    self.extend(other);
    self
  }
}

impl<T: Eq + Hash> Semigroup for HashSet<T> {
  /// Set union.
  #[inline]
  fn combine(mut self, other: Self) -> Self {
    self.extend(other);
    self
  }
}

// Numeric semigroups under addition
macro_rules! impl_semigroup_add {
  ($($t:ty),*) => {
    $(
      impl Semigroup for $t {
        #[inline]
        fn combine(self, other: Self) -> Self {
          self.wrapping_add(other)
        }
      }
    )*
  };
}

impl_semigroup_add!(
  u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

impl Semigroup for f32 {
  #[inline]
  fn combine(self, other: Self) -> Self {
    self + other
  }
}

impl Semigroup for f64 {
  #[inline]
  fn combine(self, other: Self) -> Self {
    self + other
  }
}

impl Semigroup for bool {
  /// Boolean AND as semigroup operation (all must be true).
  #[inline]
  fn combine(self, other: Self) -> Self {
    self && other
  }
}

impl Semigroup for () {
  #[inline]
  fn combine(self, _other: Self) -> Self {}
}

impl<A: Semigroup, B: Semigroup> Semigroup for (A, B) {
  #[inline]
  fn combine(self, other: Self) -> Self {
    (self.0.combine(other.0), self.1.combine(other.1))
  }
}

impl<A: Semigroup, B: Semigroup, C: Semigroup> Semigroup for (A, B, C) {
  #[inline]
  fn combine(self, other: Self) -> Self {
    (
      self.0.combine(other.0),
      self.1.combine(other.1),
      self.2.combine(other.2),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod string_semigroup {
    use super::*;

    #[test]
    fn combine_concatenates() {
      assert_eq!(
        "hello".to_string().combine(" world".to_string()),
        "hello world"
      );
    }

    #[test]
    fn combine_with_empty_is_identity_like() {
      let s = "test".to_string();
      assert_eq!(s.clone().combine(String::new()), s);
    }

    #[test]
    fn associativity_law() {
      let a = "a".to_string();
      let b = "b".to_string();
      let c = "c".to_string();
      assert_eq!(
        a.clone().combine(b.clone()).combine(c.clone()),
        a.combine(b.combine(c))
      );
    }
  }

  mod vec_semigroup {
    use super::*;

    #[test]
    fn combine_concatenates() {
      assert_eq!(vec![1, 2].combine(vec![3, 4]), vec![1, 2, 3, 4]);
    }

    #[test]
    fn associativity_law() {
      let a = vec![1];
      let b = vec![2];
      let c = vec![3];
      assert_eq!(
        a.clone().combine(b.clone()).combine(c.clone()),
        a.combine(b.combine(c))
      );
    }

    #[test]
    fn combine_with_empty() {
      let v = vec![1, 2, 3];
      assert_eq!(v.clone().combine(vec![]), v);
    }
  }

  mod duration_semigroup {
    use super::*;
    use std::time::Duration;

    #[test]
    fn combine_adds_durations() {
      let a = Duration::from_secs(1);
      let b = Duration::from_secs(2);
      assert_eq!(a.combine(b), Duration::from_secs(3));
    }

    #[test]
    fn saturating_on_overflow() {
      let max = Duration::MAX;
      let one = Duration::from_secs(1);
      assert_eq!(max.combine(one), Duration::MAX);
    }
  }

  mod ordering_semigroup {
    use super::*;

    #[rstest]
    #[case::less_wins(Ordering::Less, Ordering::Greater, Ordering::Less)]
    #[case::greater_wins(Ordering::Greater, Ordering::Less, Ordering::Greater)]
    #[case::equal_defers(Ordering::Equal, Ordering::Less, Ordering::Less)]
    #[case::equal_equal(Ordering::Equal, Ordering::Equal, Ordering::Equal)]
    fn combine_first_non_equal_wins(
      #[case] first: Ordering,
      #[case] second: Ordering,
      #[case] expected: Ordering,
    ) {
      assert_eq!(first.combine(second), expected);
    }

    #[test]
    fn associativity_law() {
      for a in [Ordering::Less, Ordering::Equal, Ordering::Greater] {
        for b in [Ordering::Less, Ordering::Equal, Ordering::Greater] {
          for c in [Ordering::Less, Ordering::Equal, Ordering::Greater] {
            assert_eq!(a.combine(b).combine(c), a.combine(b.combine(c)));
          }
        }
      }
    }
  }

  mod option_semigroup {
    use super::*;

    #[test]
    fn both_some_combines_inner() {
      assert_eq!(Some(1i32).combine(Some(2)), Some(3));
    }

    #[test]
    fn some_none_returns_some() {
      assert_eq!(Some(5i32).combine(None), Some(5));
    }

    #[test]
    fn none_some_returns_some() {
      assert_eq!(None::<i32>.combine(Some(7)), Some(7));
    }

    #[test]
    fn none_none_returns_none() {
      assert_eq!(None::<i32>.combine(None), None);
    }
  }

  mod numeric_semigroups {
    use super::*;

    #[rstest]
    #[case::u32(1u32, 2u32, 3u32)]
    #[case::i32(-1i32, 1i32, 0i32)]
    #[case::usize(10usize, 20usize, 30usize)]
    fn combine_adds<T: Semigroup + PartialEq + std::fmt::Debug>(
      #[case] a: T,
      #[case] b: T,
      #[case] expected: T,
    ) {
      assert_eq!(a.combine(b), expected);
    }

    #[test]
    fn wrapping_on_overflow() {
      assert_eq!(u8::MAX.combine(1u8), 0u8);
    }
  }

  mod bool_semigroup {
    use super::*;

    #[rstest]
    #[case::both_true(true, true, true)]
    #[case::first_false(false, true, false)]
    #[case::second_false(true, false, false)]
    #[case::both_false(false, false, false)]
    fn combine_is_and(#[case] a: bool, #[case] b: bool, #[case] expected: bool) {
      assert_eq!(a.combine(b), expected);
    }
  }

  mod tuple_semigroup {
    use super::*;

    #[test]
    fn pair_combines_componentwise() {
      let a = (1i32, "hello".to_string());
      let b = (2i32, " world".to_string());
      assert_eq!(a.combine(b), (3, "hello world".to_string()));
    }

    #[test]
    fn triple_combines_componentwise() {
      let a = (1i32, 2i32, 3i32);
      let b = (10i32, 20i32, 30i32);
      assert_eq!(a.combine(b), (11, 22, 33));
    }
  }

  mod combine_all_fn {
    use super::*;

    #[test]
    fn empty_returns_none() {
      let empty: Vec<i32> = vec![];
      assert_eq!(combine_all(empty), None);
    }

    #[test]
    fn single_element() {
      assert_eq!(combine_all(vec![42i32]), Some(42));
    }

    #[test]
    fn multiple_elements() {
      assert_eq!(combine_all(vec![1i32, 2, 3, 4]), Some(10));
    }

    #[test]
    fn strings() {
      let words = vec!["hello".to_string(), " ".to_string(), "world".to_string()];
      assert_eq!(combine_all(words), Some("hello world".to_string()));
    }
  }

  mod combine_all_or_fn {
    use super::*;

    #[test]
    fn empty_iterator_returns_first() {
      let empty: Vec<i32> = vec![];
      assert_eq!(combine_all_or(100i32, empty), 100);
    }

    #[test]
    fn non_empty_combines_with_first() {
      assert_eq!(combine_all_or(1i32, vec![2, 3, 4]), 10);
    }
  }

  mod repeat_fn {
    use super::*;

    #[test]
    fn zero_returns_none() {
      assert_eq!(repeat(5i32, 0), None);
    }

    #[test]
    fn one_returns_value() {
      assert_eq!(repeat(5i32, 1), Some(5));
    }

    #[test]
    fn multiple_combines() {
      assert_eq!(repeat(3i32, 4), Some(12)); // 3 + 3 + 3 + 3
    }

    #[test]
    fn string_repeat() {
      assert_eq!(repeat("ab".to_string(), 3), Some("ababab".to_string()));
    }
  }

  mod set_semigroups {
    use super::*;

    #[test]
    fn hashset_union() {
      let a: HashSet<i32> = [1, 2].into_iter().collect();
      let b: HashSet<i32> = [2, 3].into_iter().collect();
      let result = a.combine(b);
      assert_eq!(result.len(), 3);
      assert!(result.contains(&1) && result.contains(&2) && result.contains(&3));
    }

    #[test]
    fn btreeset_union() {
      let a: BTreeSet<i32> = [1, 2].into_iter().collect();
      let b: BTreeSet<i32> = [2, 3].into_iter().collect();
      let result: Vec<_> = a.combine(b).into_iter().collect();
      assert_eq!(result, vec![1, 2, 3]);
    }
  }

  mod btreemap_semigroup {
    use super::*;

    #[test]
    fn combine_merges_disjoint_maps() {
      let mut a = BTreeMap::new();
      a.insert("x", 1_i32);
      let mut b = BTreeMap::new();
      b.insert("y", 2_i32);
      let result = a.combine(b);
      assert_eq!(result.get("x"), Some(&1));
      assert_eq!(result.get("y"), Some(&2));
    }

    #[test]
    fn combine_merges_duplicate_keys_with_inner_semigroup() {
      let mut a = BTreeMap::new();
      a.insert("k", 10_i32);
      let mut b = BTreeMap::new();
      b.insert("k", 5_i32);
      let result = a.combine(b);
      assert_eq!(result.get("k"), Some(&15));
    }
  }

  mod hashmap_semigroup {
    use super::*;

    #[test]
    fn combine_merges_disjoint_maps() {
      let mut a = HashMap::new();
      a.insert("a", 1_i32);
      let mut b = HashMap::new();
      b.insert("b", 2_i32);
      let result = a.combine(b);
      assert_eq!(result.get("a"), Some(&1));
      assert_eq!(result.get("b"), Some(&2));
    }

    #[test]
    fn combine_merges_duplicate_keys_with_inner_semigroup() {
      let mut a = HashMap::new();
      a.insert("k", 3_i32);
      let mut b = HashMap::new();
      b.insert("k", 7_i32);
      let result = a.combine(b);
      assert_eq!(result.get("k"), Some(&10));
    }
  }

  mod float_semigroups {
    use super::*;

    #[test]
    fn f32_combine_adds() {
      assert_eq!(1.5_f32.combine(2.5_f32), 4.0_f32);
    }

    #[test]
    fn f64_combine_adds() {
      assert_eq!(1.0_f64.combine(2.0_f64), 3.0_f64);
    }
  }

  mod combine_ref_fn {
    use super::*;

    #[test]
    fn combine_ref_borrows_both_sides() {
      let a = 5_i32;
      let b = 10_i32;
      assert_eq!(a.combine_ref(&b), 15);
    }

    #[test]
    fn combine_ref_string_concatenates() {
      let a = "hello".to_string();
      let b = " world".to_string();
      assert_eq!(a.combine_ref(&b), "hello world");
    }
  }

  mod combine_free_fn {
    use super::*;

    #[test]
    fn combine_fn_delegates_to_semigroup() {
      assert_eq!(combine(3_i32, 7_i32), 10);
    }

    #[test]
    fn combine_fn_for_string() {
      assert_eq!(
        combine("ab".to_string(), "cd".to_string()),
        "abcd".to_string()
      );
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn associativity_for_integers() {
      for a in 0i32..5 {
        for b in 0i32..5 {
          for c in 0i32..5 {
            assert_eq!(
              a.combine(b).combine(c),
              a.combine(b.combine(c)),
              "failed for a={a}, b={b}, c={c}"
            );
          }
        }
      }
    }

    #[test]
    fn associativity_for_unit() {
      // Trivially true, but let's verify the impl works
      assert_eq!(().combine(()).combine(()), ().combine(().combine(())));
    }
  }

  // ── Property-based associativity (proptest) ────────────────────────────────

  mod property_laws {
    use super::*;
    use proptest::prelude::*;

    proptest! {
      /// Semigroup associativity for String (proptest, 100 cases).
      /// `(a ++ b) ++ c = a ++ (b ++ c)` for all a, b, c: String.
      #[test]
      fn string_semigroup_associativity(
        a in "[a-z]{0,10}",
        b in "[a-z]{0,10}",
        c in "[a-z]{0,10}",
      ) {
        let lhs = a.clone().combine(b.clone()).combine(c.clone());
        let rhs = a.combine(b.combine(c));
        prop_assert_eq!(lhs, rhs);
      }

      /// `Vec<i32>` concatenation is associative.
      #[test]
      fn vec_semigroup_associativity(
        a in proptest::collection::vec(0i32..100, 0..5),
        b in proptest::collection::vec(0i32..100, 0..5),
        c in proptest::collection::vec(0i32..100, 0..5),
      ) {
        let lhs = a.clone().combine(b.clone()).combine(c.clone());
        let rhs = a.combine(b.combine(c));
        prop_assert_eq!(lhs, rhs);
      }

      /// `i32` addition semigroup is associative.
      #[test]
      fn i32_semigroup_associativity(a: i32, b: i32, c: i32) {
        let lhs = a.combine(b).combine(c);
        let rhs = a.combine(b.combine(c));
        prop_assert_eq!(lhs, rhs);
      }
    }
  }
}
