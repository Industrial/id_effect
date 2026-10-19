//! [`TPriorityQueue`] — a transactional min-priority queue in one [`TRef`].

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::{Outcome, Stm, TRef};

/// Transactional priority queue (Effect.ts `TPriorityQueue`): [`TPriorityQueue::take`] removes
/// the **smallest** element by [`Ord`] and retries (parks) while the queue is empty. Wrap
/// elements in [`Reverse`] for largest-first order.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::stm::{TPriorityQueue, commit};
///
/// let Exit::Success(q) = commit(TPriorityQueue::<u32>::empty()).block_on() else { unreachable!() };
/// let q2 = q.clone();
/// let stm = q.offer_all::<()>(vec![3, 1, 2]).flat_map(move |_| q2.take());
/// assert!(matches!(commit(stm).block_on(), Exit::Success(1)));
/// ```
#[derive(Clone)]
pub struct TPriorityQueue<A> {
  inner: TRef<BinaryHeap<Reverse<A>>>,
}

impl<A> TPriorityQueue<A>
where
  A: Clone + Ord + Send + Sync + 'static,
{
  /// Empty queue.
  pub fn empty() -> Stm<TPriorityQueue<A>, ()> {
    TPriorityQueue::from_values(Vec::new())
  }

  /// Queue holding `items`.
  pub fn from_values(items: impl IntoIterator<Item = A>) -> Stm<TPriorityQueue<A>, ()> {
    TRef::make(items.into_iter().map(Reverse).collect::<BinaryHeap<_>>())
      .map(|inner| TPriorityQueue { inner })
  }

  /// Inserts `value`.
  pub fn offer<E: Send + 'static>(&self, value: A) -> Stm<(), E> {
    self.offer_all(vec![value])
  }

  /// Inserts every element of `values`.
  pub fn offer_all<E: Send + 'static>(&self, values: Vec<A>) -> Stm<(), E> {
    let q = self.clone();
    Stm::from_fn(move |txn| {
      let mut heap = q.inner.read_txn(txn);
      heap.extend(values.iter().cloned().map(Reverse));
      q.inner.set_txn(txn, heap);
      Outcome::Done(())
    })
  }

  /// Removes and returns the smallest element; retries (parks until an offer) while empty.
  pub fn take<E: Send + 'static>(&self) -> Stm<A, E> {
    let q = self.clone();
    Stm::from_fn(move |txn| {
      let mut heap = q.inner.read_txn(txn);
      match heap.pop() {
        Some(Reverse(x)) => {
          q.inner.set_txn(txn, heap);
          Outcome::Done(x)
        }
        None => Outcome::Retry,
      }
    })
  }

  /// Removes and returns the smallest element, or `None` when empty (never retries).
  pub fn take_option<E: Send + 'static>(&self) -> Stm<Option<A>, E> {
    self
      .take()
      .map(Some)
      .or_else(Stm::from_fn(|_| Outcome::Done(None)))
  }

  /// Removes every element, smallest first.
  pub fn take_all<E: Send + 'static>(&self) -> Stm<Vec<A>, E> {
    let q = self.clone();
    Stm::from_fn(move |txn| {
      let heap = q.inner.read_txn(txn);
      if heap.is_empty() {
        return Outcome::Done(Vec::new());
      }
      q.inner.set_txn(txn, BinaryHeap::new());
      Outcome::Done(
        heap
          .into_sorted_vec()
          .into_iter()
          .rev()
          .map(|Reverse(x)| x)
          .collect(),
      )
    })
  }

  /// The smallest element without removing it; retries while empty.
  pub fn peek<E: Send + 'static>(&self) -> Stm<A, E> {
    let q = self.clone();
    Stm::from_fn(move |txn| match q.inner.read_txn(txn).peek() {
      Some(Reverse(x)) => Outcome::Done(x.clone()),
      None => Outcome::Retry,
    })
  }

  /// Number of elements.
  pub fn size<E: Send + 'static>(&self) -> Stm<usize, E> {
    let q = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(q.inner.read_txn(txn).len()))
  }

  /// `true` when the queue has no elements.
  pub fn is_empty<E: Send + 'static>(&self) -> Stm<bool, E> {
    self.size().map(|n| n == 0)
  }
}
