//! [`TSet`] — a transactional hash set in one [`TRef`].

use std::collections::HashSet;
use std::hash::Hash;

use super::{Outcome, Stm, TRef};

/// Transactional hash set (Effect.ts `TSet`): one [`TRef`] holding a [`HashSet`] snapshot.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::stm::{TSet, commit};
///
/// let Exit::Success(set) = commit(TSet::<u32>::make()).block_on() else { unreachable!() };
/// let s = set.clone();
/// let added = set.add::<()>(1).flat_map(move |_| s.contains(&1));
/// assert!(matches!(commit(added).block_on(), Exit::Success(true)));
/// ```
#[derive(Clone)]
pub struct TSet<A> {
  inner: TRef<HashSet<A>>,
}

impl<A> TSet<A>
where
  A: Clone + Eq + Hash + Send + Sync + 'static,
{
  /// Empty transactional set.
  pub fn make() -> Stm<TSet<A>, ()> {
    TSet::from_values(Vec::new())
  }

  /// Set holding the distinct elements of `items`.
  pub fn from_values(items: impl IntoIterator<Item = A>) -> Stm<TSet<A>, ()> {
    TRef::make(items.into_iter().collect::<HashSet<A>>()).map(|inner| TSet { inner })
  }

  /// Adds `value`; `true` when it was not already present.
  pub fn add<E: Send + 'static>(&self, value: A) -> Stm<bool, E> {
    let s = self.clone();
    Stm::from_fn(move |txn| {
      let mut set = s.inner.read_txn(txn);
      if set.contains(&value) {
        return Outcome::Done(false);
      }
      set.insert(value.clone());
      s.inner.set_txn(txn, set);
      Outcome::Done(true)
    })
  }

  /// Removes `value`; `true` when it was present.
  pub fn remove<E: Send + 'static>(&self, value: &A) -> Stm<bool, E> {
    let s = self.clone();
    let value = value.clone();
    Stm::from_fn(move |txn| {
      let mut set = s.inner.read_txn(txn);
      if !set.remove(&value) {
        return Outcome::Done(false);
      }
      s.inner.set_txn(txn, set);
      Outcome::Done(true)
    })
  }

  /// `true` when `value` is in the set.
  pub fn contains<E: Send + 'static>(&self, value: &A) -> Stm<bool, E> {
    let s = self.clone();
    let value = value.clone();
    Stm::from_fn(move |txn| Outcome::Done(s.inner.read_txn(txn).contains(&value)))
  }

  /// Number of elements.
  pub fn size<E: Send + 'static>(&self) -> Stm<usize, E> {
    let s = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(s.inner.read_txn(txn).len()))
  }

  /// `true` when the set has no elements.
  pub fn is_empty<E: Send + 'static>(&self) -> Stm<bool, E> {
    self.size().map(|n| n == 0)
  }

  /// Snapshot of the elements (unordered).
  pub fn to_vec<E: Send + 'static>(&self) -> Stm<Vec<A>, E> {
    let s = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(s.inner.read_txn(txn).into_iter().collect()))
  }

  /// Removes and returns one element; retries (parks) while the set is empty.
  pub fn take_any<E: Send + 'static>(&self) -> Stm<A, E> {
    let s = self.clone();
    Stm::from_fn(move |txn| {
      let mut set = s.inner.read_txn(txn);
      let Some(x) = set.iter().next().cloned() else {
        return Outcome::Retry;
      };
      set.remove(&x);
      s.inner.set_txn(txn, set);
      Outcome::Done(x)
    })
  }
}
