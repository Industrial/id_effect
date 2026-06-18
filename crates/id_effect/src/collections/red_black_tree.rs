//! Ordered multimap — each key holds a FIFO list of values, backed by [`im::OrdMap`] (B-tree).
//!
//! Effect.ts “red-black” style duplicate-key semantics without exposing a separate RBT implementation.

use crate::Parallelism;
use im::OrdMap;
use rayon::prelude::*;
use std::borrow::Borrow;
use std::cmp::Ordering;

/// Ordered map allowing multiple values per key (stored left-to-right per insertion order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedBlackTree<K: Ord + Clone, V: Clone> {
  inner: OrdMap<K, Vec<V>>,
}

impl<K: Ord + Clone, V: Clone> RedBlackTree<K, V> {
  /// Empty multimap.
  #[inline]
  pub fn empty() -> Self {
    Self {
      inner: OrdMap::new(),
    }
  }

  /// Append `value` under `key` (does not replace existing values).
  pub fn insert(&mut self, key: K, value: V) {
    if self.inner.contains_key(&key) {
      self.inner.get_mut(&key).expect("key present").push(value);
    } else {
      self.inner.insert(key, vec![value]);
    }
  }

  /// Remove the oldest value for `key`. Returns it if any; drops the key when the list empties.
  pub fn remove_first<Q>(&mut self, key: &Q) -> Option<V>
  where
    Q: Ord + ?Sized,
    K: Borrow<Q> + Clone,
  {
    let (v, drop_key) = {
      let vec = self.inner.get_mut(key)?;
      if vec.is_empty() {
        return None;
      }
      let v = vec.remove(0);
      (v, vec.is_empty())
    };
    if drop_key {
      self.inner.remove(key);
    }
    Some(v)
  }

  /// Borrowed view of all values for `key` (empty if missing).
  pub fn find<Q>(&self, key: &Q) -> &[V]
  where
    Q: Ord + ?Sized,
    K: Borrow<Q>,
  {
    self.inner.get(key).map(|v| v.as_slice()).unwrap_or(&[])
  }

  /// `true` if any value is stored under `key`.
  #[inline]
  pub fn has<Q>(&self, key: &Q) -> bool
  where
    Q: Ord + ?Sized,
    K: Borrow<Q>,
  {
    self.inner.contains_key(key)
  }

  /// Smallest key’s first value, if any.
  pub fn first(&self) -> Option<(K, V)> {
    self
      .inner
      .get_min()
      .and_then(|(k, vs)| vs.first().map(|v| (k.clone(), v.clone())))
  }

  /// Largest key’s first value, if any.
  pub fn last(&self) -> Option<(K, V)> {
    self
      .inner
      .get_max()
      .and_then(|(k, vs)| vs.first().map(|v| (k.clone(), v.clone())))
  }

  /// All `(key, value)` pairs with `key > bound`, ascending (default [`Parallelism`]).
  pub fn greater_than<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    self.greater_than_with(Parallelism::default(), bound)
  }

  /// Like [`Self::greater_than`], sequentially.
  pub fn greater_than_serial<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized,
    K: Borrow<Q> + Ord + Clone,
  {
    self
      .inner
      .iter()
      .filter(|(k, _)| Borrow::<Q>::borrow(*k).cmp(bound) == Ordering::Greater)
      .flat_map(|(k, vs)| {
        let k = k.clone();
        vs.iter().cloned().map(move |v| (k.clone(), v))
      })
      .collect()
  }

  fn greater_than_parallel<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    let rows: Vec<(K, Vec<V>)> = self
      .inner
      .iter()
      .filter(|(k, _)| Borrow::<Q>::borrow(*k).cmp(bound) == Ordering::Greater)
      .map(|(k, vs)| (k.clone(), vs.clone()))
      .collect();
    rows
      .into_par_iter()
      .flat_map(|(k, vs)| {
        let kc = k;
        vs.into_par_iter().map(move |v| (kc.clone(), v))
      })
      .collect()
  }

  /// [`Self::greater_than`] with explicit policy.
  pub fn greater_than_with<Q>(&self, policy: Parallelism, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    let serial = self.greater_than_serial(bound);
    if policy.should_parallelize(serial.len()) {
      self.greater_than_parallel(bound)
    } else {
      serial
    }
  }

  /// Like [`Self::greater_than_with`] with [`Parallelism::ForceParallel`].
  /// Deprecated parallel alias.
  /// Deprecated parallel alias.
  /// Deprecated parallel alias.
  /// Deprecated parallel alias.
  /// Deprecated parallel alias.
  /// Deprecated; use [`Self::greater_than_with`](Parallelism::ForceParallel).
  #[deprecated(note = "use greater_than or greater_than_with(Parallelism::ForceParallel)")]
  pub fn greater_than_par<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    self.greater_than_with(Parallelism::ForceParallel, bound)
  }

  /// All `(key, value)` pairs with `key < bound`, ascending (default [`Parallelism`]).
  pub fn less_than<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    self.less_than_with(Parallelism::default(), bound)
  }

  /// Like [`Self::less_than`], sequentially.
  pub fn less_than_serial<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized,
    K: Borrow<Q> + Ord + Clone,
  {
    self
      .inner
      .iter()
      .filter(|(k, _)| Borrow::<Q>::borrow(*k).cmp(bound) == Ordering::Less)
      .flat_map(|(k, vs)| {
        let k = k.clone();
        vs.iter().cloned().map(move |v| (k.clone(), v))
      })
      .collect()
  }

  fn less_than_parallel<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    let rows: Vec<(K, Vec<V>)> = self
      .inner
      .iter()
      .filter(|(k, _)| Borrow::<Q>::borrow(*k).cmp(bound) == Ordering::Less)
      .map(|(k, vs)| (k.clone(), vs.clone()))
      .collect();
    rows
      .into_par_iter()
      .flat_map(|(k, vs)| {
        let kc = k;
        vs.into_par_iter().map(move |v| (kc.clone(), v))
      })
      .collect()
  }

  /// [`Self::less_than`] with explicit policy.
  pub fn less_than_with<Q>(&self, policy: Parallelism, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    let serial = self.less_than_serial(bound);
    if policy.should_parallelize(serial.len()) {
      self.less_than_parallel(bound)
    } else {
      serial
    }
  }

  /// Deprecated parallel alias.
  #[deprecated(note = "use less_than or less_than_with(Parallelism::ForceParallel)")]
  pub fn less_than_par<Q>(&self, bound: &Q) -> Vec<(K, V)>
  where
    Q: Ord + ?Sized + Sync,
    K: Borrow<Q> + Ord + Clone + Send + Sync,
    V: Clone + Send + Sync,
  {
    self.less_than_with(Parallelism::ForceParallel, bound)
  }

  /// `n`th pair in ascending key order with values expanded left-to-right per key (0-based).
  pub fn get_at(&self, n: usize) -> Option<(K, V)> {
    let mut i = 0usize;
    for (k, vs) in self.inner.iter() {
      for v in vs {
        if i == n {
          return Some((k.clone(), v.clone()));
        }
        i += 1;
      }
    }
    None
  }

  /// Total stored values (default [`Parallelism`]).
  pub fn size(&self) -> usize {
    self.size_with(Parallelism::default())
  }

  #[inline]
  /// Total stored values, sequentially.
  pub fn size_serial(&self) -> usize {
    self.inner.values().map(Vec::len).sum()
  }

  fn size_parallel(&self) -> usize {
    self
      .inner
      .values()
      .map(Vec::len)
      .collect::<Vec<_>>()
      .into_par_iter()
      .sum()
  }

  /// Total stored values with explicit policy.
  pub fn size_with(&self, policy: Parallelism) -> usize {
    let serial = self.size_serial();
    if policy.should_parallelize(serial) {
      self.size_parallel()
    } else {
      serial
    }
  }

  /// Deprecated parallel alias.
  #[deprecated(note = "use size or size_with(Parallelism::ForceParallel)")]
  pub fn size_par(&self) -> usize {
    self.size_with(Parallelism::ForceParallel)
  }

  /// All `(key, value)` pairs in ascending key order (default [`Parallelism`]).
  pub fn entries(&self) -> Vec<(K, V)>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    self.entries_with(Parallelism::default())
  }

  /// All pairs, sequentially.
  pub fn entries_serial(&self) -> Vec<(K, V)> {
    self
      .inner
      .iter()
      .flat_map(|(k, vs)| {
        let k = k.clone();
        vs.iter().cloned().map(move |v| (k.clone(), v))
      })
      .collect()
  }

  fn entries_parallel(&self) -> Vec<(K, V)>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    let rows: Vec<(K, Vec<V>)> = self
      .inner
      .iter()
      .map(|(k, vs)| (k.clone(), vs.clone()))
      .collect();
    rows
      .into_par_iter()
      .flat_map(|(k, vs)| {
        let kc = k;
        vs.into_par_iter().map(move |v| (kc.clone(), v))
      })
      .collect()
  }

  /// All pairs with explicit policy.
  pub fn entries_with(&self, policy: Parallelism) -> Vec<(K, V)>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    let serial = self.entries_serial();
    if policy.should_parallelize(serial.len()) {
      self.entries_parallel()
    } else {
      serial
    }
  }

  /// Deprecated parallel alias.
  #[deprecated(note = "use entries or entries_with(Parallelism::ForceParallel)")]
  pub fn entries_par(&self) -> Vec<(K, V)>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    self.entries_with(Parallelism::ForceParallel)
  }

  /// Distinct keys in ascending order.
  #[inline]
  pub fn keys(&self) -> Vec<K> {
    self.inner.keys().cloned().collect()
  }

  /// All values in key order (default [`Parallelism`]).
  pub fn values(&self) -> Vec<V>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    self.values_with(Parallelism::default())
  }

  /// All values, sequentially.
  pub fn values_serial(&self) -> Vec<V> {
    self
      .inner
      .values()
      .flat_map(|vs| vs.iter().cloned())
      .collect()
  }

  /// All values with explicit policy.
  pub fn values_with(&self, policy: Parallelism) -> Vec<V>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    self
      .entries_with(policy)
      .into_iter()
      .map(|(_, v)| v)
      .collect()
  }

  /// Deprecated parallel alias.
  #[deprecated(note = "use values or values_with(Parallelism::ForceParallel)")]
  pub fn values_par(&self) -> Vec<V>
  where
    K: Send + Sync + Clone,
    V: Send + Sync + Clone,
  {
    self.values_with(Parallelism::ForceParallel)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  #[test]
  fn rbt_insert_duplicate_key_both_values_retrievable() {
    let mut t = RedBlackTree::empty();
    t.insert("k", 1_i32);
    t.insert("k", 2);
    assert_eq!(t.find(&"k"), &[1, 2][..]);
    assert_eq!(t.size(), 2);
  }

  #[test]
  fn rbt_remove_first_leaves_second() {
    let mut t = RedBlackTree::empty();
    t.insert("k", 1_i32);
    t.insert("k", 2);
    assert_eq!(t.remove_first(&"k"), Some(1));
    assert_eq!(t.find(&"k"), &[2][..]);
    assert_eq!(t.remove_first(&"k"), Some(2));
    assert!(!t.has(&"k"));
  }

  #[test]
  fn rbt_greater_than_returns_correct_range() {
    let mut t = RedBlackTree::empty();
    t.insert(1_i32, "a");
    t.insert(3, "b");
    t.insert(5, "c");
    let gt = t.greater_than(&2);
    assert_eq!(gt, vec![(3, "b"), (5, "c")]);
  }

  #[rstest]
  #[case(1_i32, vec![(2, 20), (3, 30)])]
  #[case(2, vec![(3, 30)])]
  fn rbt_greater_than_respects_bound(#[case] bound: i32, #[case] expected: Vec<(i32, i32)>) {
    let mut t = RedBlackTree::empty();
    t.insert(1, 10);
    t.insert(2, 20);
    t.insert(3, 30);
    assert_eq!(t.greater_than(&bound), expected);
  }

  #[test]
  fn rbt_has_returns_true_when_key_present() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    assert!(t.has(&1));
    assert!(!t.has(&2));
  }

  #[test]
  fn rbt_first_and_last_return_min_and_max() {
    let mut t = RedBlackTree::empty();
    t.insert(2i32, "b");
    t.insert(1, "a");
    t.insert(3, "c");
    let (fk, fv) = t.first().unwrap();
    assert_eq!(fk, 1);
    assert_eq!(fv, "a");
    let (lk, lv) = t.last().unwrap();
    assert_eq!(lk, 3);
    assert_eq!(lv, "c");
  }

  #[test]
  fn rbt_first_and_last_empty_return_none() {
    let t = RedBlackTree::<i32, &str>::empty();
    assert_eq!(t.first(), None);
    assert_eq!(t.last(), None);
  }

  #[test]
  fn rbt_less_than_returns_correct_range() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    t.insert(3, "b");
    t.insert(5, "c");
    let lt = t.less_than(&4);
    assert_eq!(lt, vec![(1, "a"), (3, "b")]);
  }

  #[test]
  fn rbt_get_at_returns_correct_element() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    t.insert(2, "b");
    t.insert(3, "c");
    assert_eq!(t.get_at(0), Some((1, "a")));
    assert_eq!(t.get_at(2), Some((3, "c")));
    assert_eq!(t.get_at(10), None);
  }

  #[test]
  fn rbt_get_at_with_duplicate_keys() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    t.insert(1, "b");
    assert_eq!(t.get_at(0), Some((1, "a")));
    assert_eq!(t.get_at(1), Some((1, "b")));
  }

  #[test]
  fn rbt_size_counts_all_values_including_duplicates() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    t.insert(1, "b");
    t.insert(2, "c");
    assert_eq!(t.size(), 3);
  }

  #[test]
  fn rbt_entries_returns_all_pairs_in_key_order() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    t.insert(2, "b");
    assert_eq!(t.entries(), vec![(1, "a"), (2, "b")]);
  }

  #[test]
  fn rbt_default_entries_matches_serial() {
    let mut t = RedBlackTree::empty();
    t.insert(1i32, "a");
    t.insert(2, "b");
    t.insert(1, "b");
    assert_eq!(t.entries(), t.entries_serial());
    assert_eq!(t.values(), t.values_serial());
    assert_eq!(t.size(), t.size_serial());
  }

  #[test]
  fn rbt_keys_and_values_return_ordered_results() {
    let mut t = RedBlackTree::empty();
    t.insert(3i32, "c");
    t.insert(1, "a");
    t.insert(2, "b");
    assert_eq!(t.keys(), vec![1, 2, 3]);
    assert_eq!(t.values(), vec!["a", "b", "c"]);
  }
}
