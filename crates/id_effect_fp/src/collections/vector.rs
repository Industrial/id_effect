//! Persistent RRB-tree vector backed by [`im::Vector`].
//!
//! `EffectVector<A>` is a type alias for `im::Vector<A>`.  Every "mutation"
//! returns a new vector sharing structural nodes with the old one — O(log n)
//! for most operations, and amortised O(1) for push/pop at either end.
//!
//! The free functions mirror Effect.ts `Chunk`/`Array` combinators so that
//! code can be written in the same style as the TypeScript effect system.
//!
//! ## Relationship to [`MutableList`](super::mutable_list::MutableList)
//!
//! [`MutableList`] is a *synchronised, in-place* deque backed by `VecDeque`
//! inside a `Mutex`.  Use it when you need shared mutable append/prepend.
//! Use [`EffectVector`] when you want persistent, clone-on-write semantics:
//! each logical "version" of the vector is independent, enabling undo,
//! time-travel, and safe sharing across threads.

use im::Vector;
use std::iter::FromIterator;

/// Persistent RRB-tree vector — a type alias for [`im::Vector`].
pub type EffectVector<A> = Vector<A>;

// ── Constructors ──────────────────────────────────────────────────────────────

/// Empty vector.
#[inline]
pub fn empty<A: Clone>() -> EffectVector<A> {
  Vector::new()
}

/// Single-element vector.
#[inline]
pub fn of<A: Clone>(value: A) -> EffectVector<A> {
  Vector::unit(value)
}

/// Collect an iterator into a vector.
#[inline]
pub fn from_iter<A: Clone, I: IntoIterator<Item = A>>(iter: I) -> EffectVector<A> {
  Vector::from_iter(iter)
}

/// Convert a `Vec<A>` into a persistent vector.
#[inline]
pub fn from_vec<A: Clone>(v: Vec<A>) -> EffectVector<A> {
  Vector::from_iter(v)
}

// ── Queries ───────────────────────────────────────────────────────────────────

/// Number of elements.
#[inline]
pub fn length<A: Clone>(v: &EffectVector<A>) -> usize {
  v.len()
}

/// `true` when the vector contains no elements.
#[inline]
pub fn is_empty<A: Clone>(v: &EffectVector<A>) -> bool {
  v.is_empty()
}

/// Borrow the element at `index`, returning `None` when out of bounds.
#[inline]
pub fn get<A: Clone>(v: &EffectVector<A>, index: usize) -> Option<&A> {
  v.get(index)
}

/// Borrow the first element.
#[inline]
pub fn head<A: Clone>(v: &EffectVector<A>) -> Option<&A> {
  v.front()
}

/// Borrow the last element.
#[inline]
pub fn last<A: Clone>(v: &EffectVector<A>) -> Option<&A> {
  v.back()
}

// ── Transformations ───────────────────────────────────────────────────────────

/// Return a new vector with `value` appended at the end.
#[inline]
pub fn append<A: Clone>(v: EffectVector<A>, value: A) -> EffectVector<A> {
  let mut out = v;
  out.push_back(value);
  out
}

/// Return a new vector with `value` prepended at the front.
#[inline]
pub fn prepend<A: Clone>(v: EffectVector<A>, value: A) -> EffectVector<A> {
  let mut out = v;
  out.push_front(value);
  out
}

/// Return a new vector with the last element removed; the original is unchanged.
#[inline]
pub fn pop<A: Clone>(v: EffectVector<A>) -> (EffectVector<A>, Option<A>) {
  let mut out = v;
  let last = out.pop_back();
  (out, last)
}

/// Return a new vector with the first element removed; the original is unchanged.
#[inline]
pub fn shift<A: Clone>(v: EffectVector<A>) -> (EffectVector<A>, Option<A>) {
  let mut out = v;
  let first = out.pop_front();
  (out, first)
}

/// Apply `f` to every element, returning a new vector.
///
/// Mirrors Effect.ts `ReadonlyArray.map`.
#[inline]
pub fn map<A: Clone, B: Clone>(v: EffectVector<A>, f: impl Fn(A) -> B) -> EffectVector<B> {
  v.into_iter().map(f).collect()
}

/// Flat-map: apply `f` to every element and concatenate the resulting vectors.
#[inline]
pub fn flat_map<A: Clone, B: Clone>(
  v: EffectVector<A>,
  f: impl Fn(A) -> EffectVector<B>,
) -> EffectVector<B> {
  v.into_iter().flat_map(f).collect()
}

/// Keep only elements satisfying `pred`.
#[inline]
pub fn filter<A: Clone>(v: EffectVector<A>, pred: impl Fn(&A) -> bool) -> EffectVector<A> {
  v.into_iter().filter(|a| pred(a)).collect()
}

/// Fold left over the vector.
#[inline]
pub fn reduce<A: Clone, Acc>(v: EffectVector<A>, init: Acc, f: impl Fn(Acc, A) -> Acc) -> Acc {
  v.into_iter().fold(init, f)
}

/// Concatenate two vectors (O(log n)).
#[inline]
pub fn concat<A: Clone>(left: EffectVector<A>, right: EffectVector<A>) -> EffectVector<A> {
  left + right
}

/// Split the vector into two at `index`; elements before `index` go left.
#[inline]
pub fn split_at<A: Clone>(v: EffectVector<A>, index: usize) -> (EffectVector<A>, EffectVector<A>) {
  v.split_at(index)
}

/// Materialise as a `Vec<A>`.
#[inline]
pub fn to_vec<A: Clone>(v: EffectVector<A>) -> Vec<A> {
  v.into_iter().collect()
}

/// Reverse the vector.
#[inline]
pub fn reverse<A: Clone>(v: EffectVector<A>) -> EffectVector<A> {
  v.into_iter().rev().collect()
}

/// Return a new vector with the element at `index` replaced by `f(old)`.
/// Indices out of bounds return the original vector unchanged.
#[inline]
pub fn modify<A: Clone>(
  v: EffectVector<A>,
  index: usize,
  f: impl FnOnce(A) -> A,
) -> EffectVector<A> {
  if index >= v.len() {
    return v;
  }
  let mut out = v;
  let old = out.remove(index);
  out.insert(index, f(old));
  out
}

// ── Predicates ────────────────────────────────────────────────────────────────

/// `true` when at least one element satisfies `pred`.
#[inline]
pub fn some<A: Clone>(v: &EffectVector<A>, pred: impl Fn(&A) -> bool) -> bool {
  v.iter().any(pred)
}

/// `true` when every element satisfies `pred`.
#[inline]
pub fn every<A: Clone>(v: &EffectVector<A>, pred: impl Fn(&A) -> bool) -> bool {
  v.iter().all(pred)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  mod constructors {
    use super::*;

    #[test]
    fn empty_gives_length_zero() {
      let v: EffectVector<i32> = empty();
      assert_eq!(length(&v), 0);
      assert!(is_empty(&v));
    }

    #[test]
    fn of_gives_single_element() {
      let v = of(42_i32);
      assert_eq!(length(&v), 1);
      assert_eq!(get(&v, 0), Some(&42));
    }

    #[test]
    fn from_vec_preserves_order() {
      let v = from_vec(vec![1_i32, 2, 3]);
      assert_eq!(to_vec(v), vec![1, 2, 3]);
    }
  }

  mod push_pop {
    use super::*;

    #[test]
    fn append_adds_to_back() {
      let v = from_vec(vec![1_i32, 2]);
      let v2 = append(v, 3);
      assert_eq!(last(&v2), Some(&3));
      assert_eq!(length(&v2), 3);
    }

    #[test]
    fn prepend_adds_to_front() {
      let v = from_vec(vec![2_i32, 3]);
      let v2 = prepend(v, 1);
      assert_eq!(head(&v2), Some(&1));
      assert_eq!(length(&v2), 3);
    }

    #[test]
    fn pop_removes_last() {
      let v = from_vec(vec![1_i32, 2, 3]);
      let (v2, popped) = pop(v);
      assert_eq!(popped, Some(3));
      assert_eq!(length(&v2), 2);
    }

    #[test]
    fn shift_removes_first() {
      let v = from_vec(vec![10_i32, 20, 30]);
      let (v2, first) = shift(v);
      assert_eq!(first, Some(10));
      assert_eq!(head(&v2), Some(&20));
    }
  }

  mod transformations {
    use super::*;

    #[rstest]
    #[case(vec![1, 2, 3], vec![2, 4, 6])]
    #[case(vec![], vec![])]
    fn map_doubles_each_element(#[case] input: Vec<i32>, #[case] expected: Vec<i32>) {
      let v = from_vec(input);
      assert_eq!(to_vec(map(v, |x| x * 2)), expected);
    }

    #[test]
    fn filter_keeps_evens() {
      let v = from_vec(vec![1, 2, 3, 4, 5]);
      assert_eq!(to_vec(filter(v, |x| x % 2 == 0)), vec![2, 4]);
    }

    #[test]
    fn concat_joins_vectors() {
      let a = from_vec(vec![1, 2]);
      let b = from_vec(vec![3, 4]);
      assert_eq!(to_vec(concat(a, b)), vec![1, 2, 3, 4]);
    }

    #[test]
    fn split_at_divides_correctly() {
      let v = from_vec(vec![1, 2, 3, 4]);
      let (l, r) = split_at(v, 2);
      assert_eq!(to_vec(l), vec![1, 2]);
      assert_eq!(to_vec(r), vec![3, 4]);
    }

    #[test]
    fn reverse_flips_order() {
      let v = from_vec(vec![1, 2, 3]);
      assert_eq!(to_vec(reverse(v)), vec![3, 2, 1]);
    }

    #[test]
    fn modify_replaces_element_at_index() {
      let v = from_vec(vec![10, 20, 30]);
      let v2 = modify(v, 1, |x| x * 3);
      assert_eq!(to_vec(v2), vec![10, 60, 30]);
    }

    #[test]
    fn modify_out_of_bounds_returns_unchanged() {
      let v = from_vec(vec![1, 2]);
      let v2 = modify(v.clone(), 99, |x| x + 1);
      assert_eq!(to_vec(v2), to_vec(v));
    }
  }

  mod predicates {
    use super::*;

    #[test]
    fn some_finds_element_matching_predicate() {
      let v = from_vec(vec![1, 2, 3]);
      assert!(some(&v, |x| *x == 2));
      assert!(!some(&v, |x| *x > 10));
    }

    #[test]
    fn every_checks_all_elements() {
      let v = from_vec(vec![2, 4, 6]);
      assert!(every(&v, |x| x % 2 == 0));
      assert!(!every(&v, |x| *x > 3));
    }
  }

  mod persistence {
    use super::*;

    #[test]
    fn original_unchanged_after_append() {
      let v1 = from_vec(vec![1, 2, 3]);
      let v2 = append(v1.clone(), 4);
      assert_eq!(length(&v1), 3);
      assert_eq!(length(&v2), 4);
    }

    #[test]
    fn flat_map_expands_each_element() {
      let v = from_vec(vec![1, 2, 3]);
      let result = flat_map(v, |x| from_vec(vec![x, x * 10]));
      assert_eq!(to_vec(result), vec![1, 10, 2, 20, 3, 30]);
    }
  }
}
