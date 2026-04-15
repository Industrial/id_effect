//! **Monoid** — a semigroup with an identity element.
//!
//! A monoid extends a semigroup by adding an identity element that is neutral
//! under the combine operation.
//!
//! ## Definition
//!
//! ```text
//! MONOID[A] ::= (Semigroup[A], empty: A)
//! ```
//!
//! ## Laws
//!
//! - **Associativity** (from Semigroup): `combine(combine(a, b), c) = combine(a, combine(b, c))`
//! - **Left identity**: `combine(empty, a) = a`
//! - **Right identity**: `combine(a, empty) = a`
//!
//! ## Examples in this system
//!
//! - `String` with `""` as identity
//! - `Vec<T>` with `[]` as identity
//! - `Duration` with `Duration::ZERO` as identity
//! - `Option<A>` with `None` as identity (where `A: Semigroup`)
//! - Numeric types with `0` as identity (under addition)
//!
//! ## Relationship to Stratum 0 & 1
//!
//! - Extends: [`Semigroup`](super::semigroup::Semigroup)
//! - Uses: [`Unit`](super::super::foundation::unit) conceptually (empty is "unit" of the operation)

use super::semigroup::Semigroup;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::Hash;

/// A semigroup with an identity element.
///
/// # Laws
///
/// Implementations must satisfy:
///
/// ```text
/// combine(empty(), a) = a  // Left identity
/// combine(a, empty()) = a  // Right identity
/// ```
///
/// Plus all semigroup laws (associativity).
///
/// # Examples
///
/// ```rust
/// use id_effect::algebra::{Monoid, Semigroup};
///
/// // String monoid
/// let empty = String::empty();
/// let s = "hello".to_string();
/// assert_eq!(empty.clone().combine(s.clone()), s);
/// assert_eq!(s.clone().combine(empty), s);
/// ```
pub trait Monoid: Semigroup {
  /// The identity element for the combine operation.
  fn empty() -> Self;

  /// Check if this value is the identity element.
  fn is_empty(&self) -> bool
  where
    Self: PartialEq,
  {
    *self == Self::empty()
  }
}

/// Get the identity element for a monoid (free function).
#[inline]
pub fn empty<A: Monoid>() -> A {
  A::empty()
}

/// Combine all values in an iterator, returning `empty()` if the iterator is empty.
///
/// This is `fold` with the monoid operation starting from identity.
pub fn concat<A: Monoid>(iter: impl IntoIterator<Item = A>) -> A {
  iter.into_iter().fold(A::empty(), |acc, x| acc.combine(x))
}

/// Combine all values by reference, cloning as needed.
pub fn concat_ref<'a, A: Monoid + Clone + 'a>(iter: impl IntoIterator<Item = &'a A>) -> A {
  iter
    .into_iter()
    .fold(A::empty(), |acc, x| acc.combine(x.clone()))
}

/// Repeat a value `n` times, returning `empty()` if `n == 0`.
pub fn repeat<A: Monoid + Clone>(value: A, n: usize) -> A {
  if n == 0 {
    return A::empty();
  }
  let mut result = value.clone();
  for _ in 1..n {
    result = result.combine(value.clone());
  }
  result
}

/// Power of a monoid element: `a^n` using repeated squaring.
///
/// More efficient than `repeat` for large `n`.
pub fn power<A: Monoid + Clone>(mut base: A, mut exp: usize) -> A {
  if exp == 0 {
    return A::empty();
  }

  let mut result = A::empty();
  let mut first = true;

  while exp > 0 {
    if exp & 1 == 1 {
      if first {
        result = base.clone();
        first = false;
      } else {
        result = result.combine(base.clone());
      }
    }
    exp >>= 1;
    if exp > 0 {
      base = base.clone().combine(base);
    }
  }

  result
}

// ── Instances ────────────────────────────────────────────────────────────────

impl Monoid for String {
  #[inline]
  fn empty() -> Self {
    String::new()
  }
}

impl<T> Monoid for Vec<T> {
  #[inline]
  fn empty() -> Self {
    Vec::new()
  }
}

impl Monoid for std::time::Duration {
  #[inline]
  fn empty() -> Self {
    std::time::Duration::ZERO
  }
}

impl Monoid for Ordering {
  /// `Equal` is the identity: `combine(Equal, x) = x`.
  #[inline]
  fn empty() -> Self {
    Ordering::Equal
  }
}

impl<A: Semigroup> Monoid for Option<A> {
  /// `None` is the identity for Option's semigroup.
  #[inline]
  fn empty() -> Self {
    None
  }
}

impl<K: Ord, V: Semigroup + Clone> Monoid for BTreeMap<K, V> {
  #[inline]
  fn empty() -> Self {
    BTreeMap::new()
  }
}

impl<K: Eq + Hash, V: Semigroup + Clone> Monoid for HashMap<K, V> {
  #[inline]
  fn empty() -> Self {
    HashMap::new()
  }
}

impl<T: Ord> Monoid for BTreeSet<T> {
  #[inline]
  fn empty() -> Self {
    BTreeSet::new()
  }
}

impl<T: Eq + Hash> Monoid for HashSet<T> {
  #[inline]
  fn empty() -> Self {
    HashSet::new()
  }
}

// Numeric monoids under addition
macro_rules! impl_monoid_add {
  ($($t:ty => $zero:expr),*) => {
    $(
      impl Monoid for $t {
        #[inline]
        fn empty() -> Self {
          $zero
        }
      }
    )*
  };
}

impl_monoid_add!(
  u8 => 0,
  u16 => 0,
  u32 => 0,
  u64 => 0,
  u128 => 0,
  usize => 0,
  i8 => 0,
  i16 => 0,
  i32 => 0,
  i64 => 0,
  i128 => 0,
  isize => 0,
  f32 => 0.0,
  f64 => 0.0
);

impl Monoid for bool {
  /// `true` is the identity for AND.
  #[inline]
  fn empty() -> Self {
    true
  }
}

impl Monoid for () {
  #[inline]
  fn empty() -> Self {}
}

impl<A: Monoid, B: Monoid> Monoid for (A, B) {
  #[inline]
  fn empty() -> Self {
    (A::empty(), B::empty())
  }
}

impl<A: Monoid, B: Monoid, C: Monoid> Monoid for (A, B, C) {
  #[inline]
  fn empty() -> Self {
    (A::empty(), B::empty(), C::empty())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod string_monoid {
    use super::*;

    #[test]
    fn empty_is_empty_string() {
      assert_eq!(String::empty(), "");
    }

    #[test]
    fn left_identity() {
      let s = "hello".to_string();
      assert_eq!(String::empty().combine(s.clone()), s);
    }

    #[test]
    fn right_identity() {
      let s = "world".to_string();
      assert_eq!(s.clone().combine(String::empty()), s);
    }

    #[test]
    fn is_empty_for_empty_string() {
      assert!(String::empty().is_empty());
      assert!(!("x".to_string()).is_empty());
    }
  }

  mod vec_monoid {
    use super::*;

    #[test]
    fn empty_is_empty_vec() {
      assert_eq!(Vec::<i32>::empty(), Vec::<i32>::new());
    }

    #[test]
    fn left_identity() {
      let v = vec![1, 2, 3];
      assert_eq!(Vec::<i32>::empty().combine(v.clone()), v);
    }

    #[test]
    fn right_identity() {
      let v = vec![4, 5, 6];
      assert_eq!(v.clone().combine(Vec::empty()), v);
    }
  }

  mod duration_monoid {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_is_zero() {
      assert_eq!(Duration::empty(), Duration::ZERO);
    }

    #[test]
    fn identity_laws() {
      let d = Duration::from_secs(5);
      assert_eq!(Duration::empty().combine(d), d);
      assert_eq!(d.combine(Duration::empty()), d);
    }
  }

  mod ordering_monoid {
    use super::*;

    #[test]
    fn empty_is_equal() {
      assert_eq!(Ordering::empty(), Ordering::Equal);
    }

    #[rstest]
    #[case::less(Ordering::Less)]
    #[case::equal(Ordering::Equal)]
    #[case::greater(Ordering::Greater)]
    fn left_identity(#[case] o: Ordering) {
      assert_eq!(Ordering::empty().combine(o), o);
    }

    #[rstest]
    #[case::less(Ordering::Less)]
    #[case::equal(Ordering::Equal)]
    #[case::greater(Ordering::Greater)]
    fn right_identity(#[case] o: Ordering) {
      assert_eq!(o.combine(Ordering::empty()), o);
    }
  }

  mod option_monoid {
    use super::*;

    #[test]
    fn empty_is_none() {
      assert_eq!(Option::<i32>::empty(), None);
    }

    #[test]
    fn left_identity() {
      let o = Some(5i32);
      assert_eq!(Option::<i32>::empty().combine(o.clone()), o);
    }

    #[test]
    fn right_identity() {
      let o = Some(10i32);
      assert_eq!(o.clone().combine(Option::empty()), o);
    }
  }

  mod numeric_monoids {
    use super::*;

    #[test]
    fn empty_is_zero() {
      assert_eq!(i32::empty(), 0);
      assert_eq!(u64::empty(), 0);
      assert_eq!(f64::empty(), 0.0);
    }

    #[rstest]
    #[case::positive(5i32)]
    #[case::negative(-3i32)]
    #[case::zero(0i32)]
    fn identity_laws(#[case] n: i32) {
      assert_eq!(i32::empty().combine(n), n);
      assert_eq!(n.combine(i32::empty()), n);
    }
  }

  mod bool_monoid {
    use super::*;

    #[test]
    fn empty_is_true() {
      assert_eq!(bool::empty(), true);
    }

    #[test]
    fn left_identity() {
      assert_eq!(bool::empty().combine(true), true);
      assert_eq!(bool::empty().combine(false), false);
    }

    #[test]
    fn right_identity() {
      assert_eq!(true.combine(bool::empty()), true);
      assert_eq!(false.combine(bool::empty()), false);
    }
  }

  mod tuple_monoid {
    use super::*;

    #[test]
    fn pair_empty() {
      assert_eq!(<(i32, String)>::empty(), (0, String::new()));
    }

    #[test]
    fn pair_identity() {
      let p = (5i32, "hello".to_string());
      assert_eq!(<(i32, String)>::empty().combine(p.clone()), p);
      assert_eq!(p.clone().combine(<(i32, String)>::empty()), p);
    }
  }

  mod concat_fn {
    use super::*;

    #[test]
    fn empty_iterator_returns_empty() {
      let empty: Vec<i32> = vec![];
      assert_eq!(concat(empty), 0);
    }

    #[test]
    fn single_element() {
      assert_eq!(concat(vec![42i32]), 42);
    }

    #[test]
    fn multiple_elements() {
      assert_eq!(concat(vec![1i32, 2, 3, 4]), 10);
    }

    #[test]
    fn strings() {
      let words = vec!["hello".to_string(), " ".to_string(), "world".to_string()];
      assert_eq!(concat(words), "hello world");
    }

    #[test]
    fn empty_strings() {
      let empty: Vec<String> = vec![];
      assert_eq!(concat(empty), "");
    }
  }

  mod repeat_fn {
    use super::*;

    #[test]
    fn zero_returns_empty() {
      assert_eq!(repeat(5i32, 0), 0);
    }

    #[test]
    fn one_returns_value() {
      assert_eq!(repeat(5i32, 1), 5);
    }

    #[test]
    fn multiple() {
      assert_eq!(repeat(3i32, 4), 12);
    }

    #[test]
    fn string_repeat() {
      assert_eq!(repeat("ab".to_string(), 3), "ababab");
    }

    #[test]
    fn string_repeat_zero() {
      assert_eq!(repeat("ab".to_string(), 0), "");
    }
  }

  mod power_fn {
    use super::*;

    #[test]
    fn power_zero_returns_empty() {
      assert_eq!(power(5i32, 0), 0);
    }

    #[test]
    fn power_one_returns_value() {
      assert_eq!(power(7i32, 1), 7);
    }

    #[test]
    fn power_two() {
      assert_eq!(power(3i32, 2), 6); // 3 + 3
    }

    #[test]
    fn power_large() {
      assert_eq!(power(1i32, 100), 100); // 1 * 100
    }

    #[test]
    fn power_string() {
      assert_eq!(power("x".to_string(), 5), "xxxxx");
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn left_identity_for_integers() {
      for a in -10i32..10 {
        assert_eq!(i32::empty().combine(a), a, "left identity failed for {a}");
      }
    }

    #[test]
    fn right_identity_for_integers() {
      for a in -10i32..10 {
        assert_eq!(a.combine(i32::empty()), a, "right identity failed for {a}");
      }
    }

    #[test]
    fn concat_is_fold_with_empty() {
      let values = vec![1i32, 2, 3, 4, 5];
      let via_concat = concat(values.clone());
      let via_fold = values.into_iter().fold(i32::empty(), |a, b| a.combine(b));
      assert_eq!(via_concat, via_fold);
    }
  }

  // ── Property-based monoid identity laws (proptest) ─────────────────────────

  mod property_laws {
    use super::*;
    use proptest::prelude::*;

    proptest! {
      /// Monoid left identity: `empty().combine(a) = a` for all a: String.
      #[test]
      fn string_monoid_left_identity(a in "[a-z]{0,20}") {
        prop_assert_eq!(String::empty().combine(a.clone()), a);
      }

      /// Monoid right identity: `a.combine(empty()) = a` for all a: String.
      #[test]
      fn string_monoid_right_identity(a in "[a-z]{0,20}") {
        prop_assert_eq!(a.clone().combine(String::empty()), a);
      }

      /// `i32` monoid left+right identity with arbitrary values.
      #[test]
      fn i32_monoid_identity(a: i32) {
        prop_assert_eq!(i32::empty().combine(a), a);
        prop_assert_eq!(a.combine(i32::empty()), a);
      }

      /// `concat` is equivalent to fold over `combine` for Vec<String>.
      #[test]
      fn vec_string_concat_equals_fold(
        v in proptest::collection::vec("[a-z]{0,5}", 0..8)
      ) {
        let via_concat = concat(v.clone());
        let via_fold = v.into_iter().fold(String::empty(), |a, b| a.combine(b));
        prop_assert_eq!(via_concat, via_fold);
      }
    }
  }

  // ── Previously uncovered types ────────────────────────────────────────────

  mod btreemap_monoid {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn empty_is_empty_map() {
      assert_eq!(BTreeMap::<i32, i32>::empty(), BTreeMap::new());
    }

    #[test]
    fn combine_merges_disjoint_maps() {
      let mut a = BTreeMap::new();
      a.insert(1, vec![10]);
      let mut b = BTreeMap::new();
      b.insert(2, vec![20]);
      let result = a.combine(b);
      assert_eq!(result[&1], vec![10]);
      assert_eq!(result[&2], vec![20]);
    }
  }

  mod hashmap_monoid {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn empty_is_empty_map() {
      assert_eq!(HashMap::<i32, i32>::empty(), HashMap::new());
    }

    #[test]
    fn combine_merges_disjoint_maps() {
      let mut a = HashMap::new();
      a.insert("x", vec![1]);
      let mut b = HashMap::new();
      b.insert("y", vec![2]);
      let result = a.combine(b);
      assert_eq!(result["x"], vec![1]);
      assert_eq!(result["y"], vec![2]);
    }
  }

  mod btreeset_monoid {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn empty_is_empty_set() {
      assert_eq!(BTreeSet::<i32>::empty(), BTreeSet::new());
    }

    #[test]
    fn combine_unions_sets() {
      let a: BTreeSet<i32> = [1, 2, 3].into_iter().collect();
      let b: BTreeSet<i32> = [3, 4, 5].into_iter().collect();
      let result = a.combine(b);
      let expected: BTreeSet<i32> = [1, 2, 3, 4, 5].into_iter().collect();
      assert_eq!(result, expected);
    }
  }

  mod hashset_monoid {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn empty_is_empty_set() {
      assert_eq!(HashSet::<i32>::empty(), HashSet::new());
    }

    #[test]
    fn combine_unions_sets() {
      let a: HashSet<i32> = [1, 2].into_iter().collect();
      let b: HashSet<i32> = [2, 3].into_iter().collect();
      let result = a.combine(b);
      let expected: HashSet<i32> = [1, 2, 3].into_iter().collect();
      assert_eq!(result, expected);
    }
  }

  mod unit_monoid {
    use super::*;

    #[test]
    fn empty_is_unit() {
      assert_eq!(<()>::empty(), ());
    }

    #[test]
    fn combine_is_unit() {
      assert_eq!(().combine(()), ());
    }
  }

  mod triple_tuple_monoid {
    use super::*;

    #[test]
    fn empty_is_triple_empty() {
      let e = <(i32, String, bool)>::empty();
      assert_eq!(e, (0, String::new(), true));
    }

    #[test]
    fn identity_laws() {
      let t = (5i32, "hi".to_string(), false);
      assert_eq!(<(i32, String, bool)>::empty().combine(t.clone()), t);
      assert_eq!(t.clone().combine(<(i32, String, bool)>::empty()), t);
    }
  }

  mod concat_ref_fn {
    use super::*;

    #[test]
    fn concat_ref_on_strings() {
      let strs = ["hello".to_string(), " ".to_string(), "world".to_string()];
      assert_eq!(concat_ref(strs.iter()), "hello world");
    }

    #[test]
    fn concat_ref_empty_gives_empty() {
      let empty: Vec<i32> = vec![];
      assert_eq!(concat_ref(empty.iter()), 0);
    }

    #[test]
    fn concat_ref_integers() {
      let ns = [1i32, 2, 3, 4, 5];
      assert_eq!(concat_ref(ns.iter()), 15);
    }
  }
}
