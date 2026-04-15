//! Ordered key–value map backed by [`im::OrdMap`] (persistent B-tree), mirroring Effect.ts ordered maps.
//!
//! Keys are ordered by [`Ord`]. Use [`RedBlackTree`](crate::collections::red_black_tree::RedBlackTree) when duplicate
//! keys must retain multiple values.

use im::OrdMap;
use std::borrow::Borrow;
use std::iter::FromIterator;

/// Persistent ordered map — alias of `im::OrdMap`.
pub type EffectSortedMap<K, V> = OrdMap<K, V>;

/// Empty map.
#[inline]
pub fn empty<K: Ord + Clone, V: Clone>() -> OrdMap<K, V> {
  OrdMap::new()
}

/// Build from an iterator of `(key, value)` pairs. Later pairs replace earlier keys.
#[inline]
pub fn from_iter<I, K, V>(iter: I) -> OrdMap<K, V>
where
  I: IntoIterator<Item = (K, V)>,
  K: Ord + Clone,
  V: Clone,
{
  OrdMap::from_iter(iter)
}

/// Cloned value for `key`, if present.
#[inline]
pub fn get<K, V, Q>(m: &OrdMap<K, V>, key: &Q) -> Option<V>
where
  Q: Ord + ?Sized,
  K: Borrow<Q> + Ord + Clone,
  V: Clone,
{
  m.get(key).cloned()
}

/// Whether `key` is present.
#[inline]
pub fn has<K, V, Q>(m: &OrdMap<K, V>, key: &Q) -> bool
where
  Q: Ord + ?Sized,
  K: Borrow<Q> + Ord + Clone,
  V: Clone,
{
  m.contains_key(key)
}

/// Insert or replace `key` → `value`. Returns updated map.
#[inline]
pub fn set<K: Ord + Clone, V: Clone>(mut m: OrdMap<K, V>, key: K, value: V) -> OrdMap<K, V> {
  m.insert(key, value);
  m
}

/// Removes `key`; returns the updated map and previous value, if any.
#[inline]
pub fn remove<K, V, Q>(mut m: OrdMap<K, V>, key: &Q) -> (OrdMap<K, V>, Option<V>)
where
  Q: Ord + ?Sized,
  K: Borrow<Q> + Ord + Clone,
  V: Clone,
{
  let old = m.remove(key);
  (m, old)
}

/// Update value at `key` with `f`; removes the key if `f` returns `None`.
pub fn modify<K, V, F>(mut m: OrdMap<K, V>, key: K, f: F) -> OrdMap<K, V>
where
  K: Ord + Clone,
  V: Clone,
  F: FnOnce(Option<V>) -> Option<V>,
{
  let old = m.remove(&key);
  if let Some(next) = f(old) {
    m.insert(key, next);
  }
  m
}

/// Like [`modify`] but looks up by borrowed key; `key_owned` is used only when re-inserting.
pub fn modify_at<K, V, Q, F>(mut m: OrdMap<K, V>, key: &Q, key_owned: K, f: F) -> OrdMap<K, V>
where
  Q: Ord + ?Sized,
  K: Borrow<Q> + Ord + Clone,
  V: Clone,
  F: FnOnce(Option<V>) -> Option<V>,
{
  let old = m.remove(key);
  if let Some(next) = f(old) {
    m.insert(key_owned, next);
  }
  m
}

/// Maps every value; key order preserved.
pub fn map_values<K, V, W, F>(m: OrdMap<K, V>, f: F) -> OrdMap<K, W>
where
  K: Ord + Clone,
  V: Clone,
  W: Clone,
  F: FnMut(V) -> W,
{
  let mut f = f;
  let mut out = OrdMap::new();
  for (k, v) in m.into_iter() {
    out.insert(k, f(v));
  }
  out
}

/// Keeps entries where `pred` holds.
pub fn filter<K, V, F>(m: OrdMap<K, V>, mut pred: F) -> OrdMap<K, V>
where
  K: Ord + Clone,
  V: Clone,
  F: FnMut(&K, &V) -> bool,
{
  let mut out = OrdMap::new();
  for (k, v) in m.into_iter() {
    if pred(&k, &v) {
      out.insert(k, v);
    }
  }
  out
}

/// Maps keys and values into a new ordered map (`fk` must preserve key ordering intent).
pub fn map<K, V, W, FK, FV>(m: OrdMap<K, V>, mut fk: FK, mut fv: FV) -> OrdMap<K, W>
where
  K: Ord + Clone,
  V: Clone,
  W: Clone,
  FK: FnMut(K) -> K,
  FV: FnMut(V) -> W,
{
  let mut out = OrdMap::new();
  for (k, v) in m.into_iter() {
    out.insert(fk(k), fv(v));
  }
  out
}

/// Left-biased union: keys only from `right` are added when absent in `left`.
pub fn union<K, V>(mut left: OrdMap<K, V>, right: OrdMap<K, V>) -> OrdMap<K, V>
where
  K: Ord + Clone,
  V: Clone,
{
  for (k, v) in right.into_iter() {
    if !left.contains_key(&k) {
      left.insert(k, v);
    }
  }
  left
}

/// All keys in ascending order.
#[inline]
pub fn keys<K: Ord + Clone, V: Clone>(m: &OrdMap<K, V>) -> Vec<K> {
  m.iter().map(|(k, _)| k.clone()).collect()
}

/// Values in key order.
#[inline]
pub fn values<K: Ord + Clone, V: Clone>(m: &OrdMap<K, V>) -> Vec<V> {
  m.iter().map(|(_, v)| v.clone()).collect()
}

/// Number of entries.
#[inline]
pub fn size<K: Ord + Clone, V: Clone>(m: &OrdMap<K, V>) -> usize {
  m.len()
}

/// Whether the map has no entries.
#[inline]
pub fn is_empty<K: Ord + Clone, V: Clone>(m: &OrdMap<K, V>) -> bool {
  m.is_empty()
}

/// Smallest key (and value), by `Ord`.
#[inline]
pub fn head<K: Ord + Clone, V: Clone>(m: &OrdMap<K, V>) -> Option<(K, V)> {
  m.get_min().map(|(k, v)| (k.clone(), v.clone()))
}

/// Largest key (and value), by `Ord`.
#[inline]
pub fn last<K: Ord + Clone, V: Clone>(m: &OrdMap<K, V>) -> Option<(K, V)> {
  m.get_max().map(|(k, v)| (k.clone(), v.clone()))
}

/// Folds over entries in key order.
pub fn reduce<K, V, Acc>(m: OrdMap<K, V>, init: Acc, mut f: impl FnMut(Acc, K, V) -> Acc) -> Acc
where
  K: Ord + Clone,
  V: Clone,
{
  let mut acc = init;
  for (k, v) in m.into_iter() {
    acc = f(acc, k, v);
  }
  acc
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  #[test]
  fn sorted_map_head_is_min_key() {
    let m = from_iter([(3_i32, "c"), (1, "a"), (2, "b")]);
    let (k, v) = head(&m).expect("non-empty");
    assert_eq!(k, 1);
    assert_eq!(v, "a");
  }

  #[test]
  fn sorted_map_last_is_max_key() {
    let m = from_iter([(3_i32, "c"), (1, "a"), (2, "b")]);
    let (k, v) = last(&m).expect("non-empty");
    assert_eq!(k, 3);
    assert_eq!(v, "c");
  }

  #[rstest]
  #[case(vec![(1_i32, 10), (2, 20)], 1, 10)]
  #[case(vec![(5_i32, 50), (3, 30), (4, 40)], 3, 30)]
  fn head_matches_minimum_key_across_build_orders(
    #[case] pairs: Vec<(i32, i32)>,
    #[case] expected_k: i32,
    #[case] expected_v: i32,
  ) {
    let m = from_iter(pairs);
    let (k, v) = head(&m).expect("non-empty");
    assert_eq!(k, expected_k);
    assert_eq!(v, expected_v);
  }

  #[test]
  fn union_prefers_left_on_conflict() {
    let a = from_iter([("x", 1_i32)]);
    let b = from_iter([("x", 99), ("y", 2)]);
    let u = union(a, b);
    assert_eq!(get(&u, "x"), Some(1));
    assert_eq!(get(&u, "y"), Some(2));
  }

  #[test]
  fn has_returns_correct_membership() {
    let m = from_iter([(1i32, "a"), (2, "b")]);
    assert!(has(&m, &1));
    assert!(!has(&m, &3));
  }

  #[test]
  fn remove_returns_old_value_and_shrinks_map() {
    let m = from_iter([(1i32, "a"), (2, "b")]);
    let (m2, old) = remove(m, &1);
    assert_eq!(old, Some("a"));
    assert!(!has(&m2, &1));
    let (m3, old2) = remove(m2, &99);
    assert_eq!(old2, None);
    assert_eq!(size(&m3), 1);
  }

  #[test]
  fn modify_inserts_when_key_missing() {
    let m = empty::<i32, i32>();
    let m2 = modify(m, 1, |_| Some(42));
    assert_eq!(get(&m2, &1), Some(42));
  }

  #[test]
  fn modify_removes_key_when_returns_none() {
    let m = from_iter([(1i32, 10i32)]);
    let m2 = modify(m, 1, |_| None);
    assert!(!has(&m2, &1));
  }

  #[test]
  fn modify_at_updates_existing_key() {
    let m = from_iter([(1i32, 10i32)]);
    let m2 = modify_at(m, &1, 1, |v| Some(v.map(|x| x * 2).unwrap_or(0)));
    assert_eq!(get(&m2, &1), Some(20));
  }

  #[test]
  fn modify_at_removes_key_when_returns_none() {
    let m = from_iter([(1i32, 10i32)]);
    let m2 = modify_at(m, &1, 1, |_| None);
    assert!(!has(&m2, &1));
  }

  #[test]
  fn map_values_transforms_all() {
    let m = from_iter([(1i32, 10i32), (2, 20)]);
    let m2 = map_values(m, |v| v * 3);
    assert_eq!(get(&m2, &1), Some(30));
    assert_eq!(get(&m2, &2), Some(60));
  }

  #[test]
  fn filter_keeps_matching_entries() {
    let m = from_iter([(1i32, 10i32), (2, 20), (3, 30)]);
    let m2 = filter(m, |k, _v| *k > 1);
    assert!(!has(&m2, &1));
    assert!(has(&m2, &2));
  }

  #[test]
  fn map_transforms_keys_and_values() {
    let m = from_iter([(1i32, 10i32), (2, 20)]);
    let m2 = map(m, |k| k + 10, |v| v * 2);
    assert_eq!(get(&m2, &11), Some(20));
    assert_eq!(get(&m2, &12), Some(40));
  }

  #[test]
  fn keys_returns_all_keys_sorted() {
    let m = from_iter([(3i32, "c"), (1, "a"), (2, "b")]);
    assert_eq!(keys(&m), vec![1, 2, 3]);
  }

  #[test]
  fn values_returns_values_in_key_order() {
    let m = from_iter([(1i32, "a"), (2, "b"), (3, "c")]);
    assert_eq!(values(&m), vec!["a", "b", "c"]);
  }

  #[test]
  fn size_and_is_empty() {
    let m = empty::<i32, i32>();
    assert!(is_empty(&m));
    assert_eq!(size(&m), 0);
    let m = set(m, 1, 10);
    assert!(!is_empty(&m));
    assert_eq!(size(&m), 1);
  }

  #[test]
  fn reduce_folds_entries_in_key_order() {
    let m = from_iter([(1i32, 10i32), (2, 20), (3, 30)]);
    let sum = reduce(m, 0, |acc, k, v| acc + k + v);
    assert_eq!(sum, 66);
  }
}
