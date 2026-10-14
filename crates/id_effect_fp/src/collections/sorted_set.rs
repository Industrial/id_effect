//! Persistent ordered set backed by [`im::OrdSet`] (B-tree).
//!
//! `EffectSortedSet<A>` is a type alias for `im::OrdSet<A>`.
//! Elements must implement `Ord + Clone`.  Every "mutation" returns a new set
//! sharing structural nodes — O(log n) per operation.
//!
//! ## Relationship to [`EffectHashSet`](super::hash_set::EffectHashSet)
//!
//! Use [`EffectHashSet`] when you only need `O(1)` membership tests and don't
//! care about element order.  Use [`EffectSortedSet`] when iteration in sorted
//! order, `min`/`max`, range queries, or set-difference in key order matters.

use im::OrdSet;

/// Persistent sorted set — a type alias for [`im::OrdSet`].
pub type EffectSortedSet<A> = OrdSet<A>;

// ── Constructors ──────────────────────────────────────────────────────────────

/// Empty set.
#[inline]
pub fn empty<A: Ord + Clone>() -> EffectSortedSet<A> {
  OrdSet::new()
}

/// Single-element set.
#[inline]
pub fn of<A: Ord + Clone>(value: A) -> EffectSortedSet<A> {
  OrdSet::unit(value)
}

/// Collect an iterator into a set (duplicates are collapsed).
#[inline]
pub fn from_iter<A: Ord + Clone, I: IntoIterator<Item = A>>(iter: I) -> EffectSortedSet<A> {
  iter.into_iter().collect()
}

// ── Queries ───────────────────────────────────────────────────────────────────

/// Number of elements.
#[inline]
pub fn size<A: Ord + Clone>(s: &EffectSortedSet<A>) -> usize {
  s.len()
}

/// `true` when the set contains no elements.
#[inline]
pub fn is_empty<A: Ord + Clone>(s: &EffectSortedSet<A>) -> bool {
  s.is_empty()
}

/// `true` when `value` is a member of the set.
#[inline]
pub fn has<A: Ord + Clone>(s: &EffectSortedSet<A>, value: &A) -> bool {
  s.contains(value)
}

/// Borrow the smallest element, if any.
#[inline]
pub fn min<A: Ord + Clone>(s: &EffectSortedSet<A>) -> Option<&A> {
  s.get_min()
}

/// Borrow the largest element, if any.
#[inline]
pub fn max<A: Ord + Clone>(s: &EffectSortedSet<A>) -> Option<&A> {
  s.get_max()
}

// ── Modifications ─────────────────────────────────────────────────────────────

/// Return a new set with `value` inserted (O(log n)).
#[inline]
pub fn insert<A: Ord + Clone>(s: EffectSortedSet<A>, value: A) -> EffectSortedSet<A> {
  s.update(value)
}

/// Return a new set with `value` removed (O(log n)).
#[inline]
pub fn remove<A: Ord + Clone>(s: EffectSortedSet<A>, value: &A) -> EffectSortedSet<A> {
  s.without(value)
}

// ── Set operations ────────────────────────────────────────────────────────────

/// Set union: elements in either set.
#[inline]
pub fn union<A: Ord + Clone>(a: EffectSortedSet<A>, b: EffectSortedSet<A>) -> EffectSortedSet<A> {
  a.union(b)
}

/// Set intersection: elements in both sets.
#[inline]
pub fn intersection<A: Ord + Clone>(
  a: EffectSortedSet<A>,
  b: EffectSortedSet<A>,
) -> EffectSortedSet<A> {
  a.intersection(b)
}

/// Set difference: elements in `a` but not in `b`.
#[inline]
pub fn difference<A: Ord + Clone>(
  a: EffectSortedSet<A>,
  b: &EffectSortedSet<A>,
) -> EffectSortedSet<A> {
  a.difference(b.clone())
}

/// `true` when `a` and `b` share no elements.
#[inline]
pub fn is_disjoint<A: Ord + Clone>(a: &EffectSortedSet<A>, b: &EffectSortedSet<A>) -> bool {
  a.clone().intersection(b.clone()).is_empty()
}

/// `true` when every element of `sub` is also in `sup`.
#[inline]
pub fn is_subset<A: Ord + Clone>(sub: &EffectSortedSet<A>, sup: &EffectSortedSet<A>) -> bool {
  sub.is_subset(sup)
}

// ── Conversions ───────────────────────────────────────────────────────────────

/// Collect all elements (in ascending order) into a `Vec<A>`.
#[inline]
pub fn to_vec<A: Ord + Clone>(s: EffectSortedSet<A>) -> Vec<A> {
  s.into_iter().collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn empty_gives_size_zero() {
      let s: EffectSortedSet<i32> = empty();
      assert_eq!(size(&s), 0);
      assert!(is_empty(&s));
    }

    #[test]
    fn of_gives_single_element() {
      let s = of(7_i32);
      assert_eq!(size(&s), 1);
      assert!(has(&s, &7));
    }

    #[test]
    fn from_iter_deduplicates() {
      let s = from_iter([3, 1, 2, 1, 3_i32]);
      assert_eq!(size(&s), 3);
    }
  }

  mod queries {
    use super::*;

    #[rstest]
    #[case(vec![5, 3, 1, 4, 2], Some(1), Some(5))]
    #[case(vec![], None, None)]
    fn min_max(
      #[case] elems: Vec<i32>,
      #[case] expected_min: Option<i32>,
      #[case] expected_max: Option<i32>,
    ) {
      let s = from_iter(elems);
      assert_eq!(min(&s).copied(), expected_min);
      assert_eq!(max(&s).copied(), expected_max);
    }
  }

  mod insert_remove {
    use super::*;

    #[test]
    fn insert_adds_new_element() {
      let s = from_iter([1_i32, 2, 3]);
      let s2 = insert(s, 4);
      assert!(has(&s2, &4));
      assert_eq!(size(&s2), 4);
    }

    #[test]
    fn insert_ignores_duplicate() {
      let s = from_iter([1_i32, 2]);
      let s2 = insert(s, 1);
      assert_eq!(size(&s2), 2);
    }

    #[test]
    fn remove_deletes_element() {
      let s = from_iter([1_i32, 2, 3]);
      let s2 = remove(s, &2);
      assert!(!has(&s2, &2));
      assert_eq!(size(&s2), 2);
    }
  }

  mod set_ops {
    use super::*;

    #[test]
    fn union_merges_both_sets() {
      let a = from_iter([1_i32, 2, 3]);
      let b = from_iter([3_i32, 4, 5]);
      let u = union(a, b);
      assert_eq!(to_vec(u), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn intersection_keeps_common_elements() {
      let a = from_iter([1_i32, 2, 3, 4]);
      let b = from_iter([2_i32, 4, 6]);
      assert_eq!(to_vec(intersection(a, b)), vec![2, 4]);
    }

    #[test]
    fn difference_removes_b_from_a() {
      let a = from_iter([1_i32, 2, 3, 4]);
      let b = from_iter([2_i32, 4]);
      assert_eq!(to_vec(difference(a, &b)), vec![1, 3]);
    }

    #[test]
    fn is_subset_when_a_inside_b() {
      let a = from_iter([2_i32, 3]);
      let b = from_iter([1_i32, 2, 3, 4]);
      assert!(is_subset(&a, &b));
      assert!(!is_subset(&b, &a));
    }

    #[test]
    fn is_disjoint_when_no_overlap() {
      let a = from_iter([1_i32, 2]);
      let b = from_iter([3_i32, 4]);
      assert!(is_disjoint(&a, &b));
      let c = from_iter([2_i32, 3]);
      assert!(!is_disjoint(&a, &c));
    }
  }

  mod order {
    use super::*;

    #[test]
    fn to_vec_produces_ascending_order() {
      let s = from_iter([5_i32, 3, 1, 4, 2]);
      assert_eq!(to_vec(s), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn persistence_original_unchanged_after_insert() {
      let s1 = from_iter([1_i32, 2, 3]);
      let s2 = insert(s1.clone(), 4);
      assert!(!has(&s1, &4));
      assert!(has(&s2, &4));
    }
  }
}
