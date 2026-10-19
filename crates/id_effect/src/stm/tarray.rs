//! [`TArray`] — a fixed-length transactional array, one [`TRef`] per element.

use std::sync::Arc;

use super::{Outcome, Stm, TRef};

/// Fixed-length transactional array (Effect.ts `TArray`).
///
/// Each element is its own [`TRef`], so transactions touching different indices never conflict
/// and a retry parks only on the elements it read. Out-of-range indices are not errors in the
/// transaction's error channel: [`TArray::get`] gives `None` and the writers give `false`.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::stm::{TArray, commit};
///
/// let Exit::Success(arr) = commit(TArray::make(vec![1, 2, 3])).block_on() else { unreachable!() };
/// let a = arr.clone();
/// let stm = arr.update::<(), _>(1, |n| n * 10).flat_map(move |_| a.to_vec());
/// assert!(matches!(commit(stm).block_on(), Exit::Success(v) if v == vec![1, 20, 3]));
/// ```
#[derive(Clone)]
pub struct TArray<A> {
  cells: Arc<[TRef<A>]>,
}

impl<A> TArray<A>
where
  A: Clone + Send + Sync + 'static,
{
  /// Array holding `values` (one fresh cell per element).
  pub fn make(values: Vec<A>) -> Stm<TArray<A>, ()> {
    Stm::from_fn(move |_| {
      Outcome::Done(TArray {
        cells: values.iter().cloned().map(TRef::new).collect(),
      })
    })
  }

  /// Number of elements (fixed at construction; not transactional).
  pub fn len(&self) -> usize {
    self.cells.len()
  }

  /// `true` when the array has no elements.
  pub fn is_empty(&self) -> bool {
    self.cells.is_empty()
  }

  /// The cell at `index`, for composing with [`TRef`] operations directly.
  pub fn cell(&self, index: usize) -> Option<&TRef<A>> {
    self.cells.get(index)
  }

  /// Element at `index`, or `None` when out of range.
  pub fn get<E: Send + 'static>(&self, index: usize) -> Stm<Option<A>, E> {
    let a = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(a.cells.get(index).map(|c| c.read_txn(txn))))
  }

  /// Writes `value` at `index`; `false` (nothing written) when out of range.
  pub fn set<E: Send + 'static>(&self, index: usize, value: A) -> Stm<bool, E> {
    let a = self.clone();
    Stm::from_fn(move |txn| match a.cells.get(index) {
      Some(c) => {
        c.set_txn(txn, value.clone());
        Outcome::Done(true)
      }
      None => Outcome::Done(false),
    })
  }

  /// Replaces the element at `index` with `f(old)`; `false` when out of range.
  pub fn update<E, F>(&self, index: usize, f: F) -> Stm<bool, E>
  where
    E: Send + 'static,
    F: Fn(A) -> A + Send + Sync + 'static,
  {
    let a = self.clone();
    Stm::from_fn(move |txn| match a.cells.get(index) {
      Some(c) => {
        let cur = c.read_txn(txn);
        c.set_txn(txn, f(cur));
        Outcome::Done(true)
      }
      None => Outcome::Done(false),
    })
  }

  /// Snapshot of every element, in index order.
  pub fn to_vec<E: Send + 'static>(&self) -> Stm<Vec<A>, E> {
    let a = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(a.cells.iter().map(|c| c.read_txn(txn)).collect()))
  }

  /// Folds every element, in index order.
  pub fn fold<B, E, F>(&self, init: B, f: F) -> Stm<B, E>
  where
    B: Clone + Send + Sync + 'static,
    E: Send + 'static,
    F: Fn(B, A) -> B + Send + Sync + 'static,
  {
    let a = self.clone();
    Stm::from_fn(move |txn| {
      let mut acc = init.clone();
      for c in a.cells.iter() {
        acc = f(acc, c.read_txn(txn));
      }
      Outcome::Done(acc)
    })
  }
}
