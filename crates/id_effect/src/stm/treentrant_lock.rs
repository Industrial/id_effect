//! [`TReentrantLock`] — a transactional read/write lock, reentrant per fiber.

use std::collections::HashMap;

use super::{Outcome, Stm, TRef};
use crate::FiberId;

/// Holders of a [`TReentrantLock`].
#[derive(Clone, Default)]
struct LockState {
  /// Fiber → number of read locks it holds.
  readers: HashMap<FiberId, usize>,
  /// The writing fiber and how many write locks it holds.
  writer: Option<(FiberId, usize)>,
}

/// Transactional read/write lock (Effect.ts `TReentrantLock`).
///
/// The owner identity is the fiber running [`commit`](super::commit): any number of fibers may
/// hold read locks while no other fiber holds the write lock; one fiber at a time holds the write
/// lock, and only while no other fiber holds a read lock. Both locks are reentrant (each acquire
/// needs a matching release), and the writer may also take read locks, and a fiber that is the
/// only reader may upgrade to the write lock. A blocked acquire retries (parks) until a release
/// changes the lock state.
///
/// ```
/// use id_effect::Exit;
/// use id_effect::stm::{TReentrantLock, commit};
///
/// let Exit::Success(lock) = commit(TReentrantLock::make()).block_on() else { unreachable!() };
/// let l = lock.clone();
/// // The same fiber takes the write lock twice (reentrancy), then releases both.
/// let stm = lock
///   .acquire_write::<()>()
///   .flat_map(move |_| l.acquire_write())
///   .flat_map({
///     let l = lock.clone();
///     move |n| l.release_write().flat_map({
///       let l = l.clone();
///       move |_| l.release_write().map(move |_| n)
///     })
///   });
/// assert!(matches!(commit(stm).block_on(), Exit::Success(2)));
/// ```
#[derive(Clone)]
pub struct TReentrantLock {
  inner: TRef<LockState>,
}

impl TReentrantLock {
  /// An unlocked lock.
  pub fn make() -> Stm<TReentrantLock, ()> {
    TRef::make(LockState::default()).map(|inner| TReentrantLock { inner })
  }

  /// Takes a read lock for the committing fiber; retries while another fiber writes. Returns
  /// the number of read locks this fiber now holds.
  pub fn acquire_read<E: Send + 'static>(&self) -> Stm<usize, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| {
      let me = txn.fiber;
      let mut st = l.inner.read_txn(txn);
      if matches!(st.writer, Some((w, _)) if w != me) {
        return Outcome::Retry;
      }
      let n = st.readers.entry(me).or_insert(0);
      *n += 1;
      let n = *n;
      l.inner.set_txn(txn, st);
      Outcome::Done(n)
    })
  }

  /// Releases one read lock of the committing fiber; `false` when it held none.
  pub fn release_read<E: Send + 'static>(&self) -> Stm<bool, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| {
      let me = txn.fiber;
      let mut st = l.inner.read_txn(txn);
      match st.readers.get_mut(&me) {
        None => return Outcome::Done(false),
        Some(1) => {
          st.readers.remove(&me);
        }
        Some(n) => *n -= 1,
      }
      l.inner.set_txn(txn, st);
      Outcome::Done(true)
    })
  }

  /// Takes the write lock for the committing fiber; retries while another fiber holds a read
  /// or write lock. Returns the number of write locks this fiber now holds.
  pub fn acquire_write<E: Send + 'static>(&self) -> Stm<usize, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| {
      let me = txn.fiber;
      let mut st = l.inner.read_txn(txn);
      let other_writer = matches!(st.writer, Some((w, _)) if w != me);
      let other_reader = st.readers.keys().any(|f| *f != me);
      if other_writer || other_reader {
        return Outcome::Retry;
      }
      let n = st.writer.map_or(0, |(_, n)| n) + 1;
      st.writer = Some((me, n));
      l.inner.set_txn(txn, st);
      Outcome::Done(n)
    })
  }

  /// Releases one write lock of the committing fiber; `false` when it held none.
  pub fn release_write<E: Send + 'static>(&self) -> Stm<bool, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| {
      let me = txn.fiber;
      let mut st = l.inner.read_txn(txn);
      match st.writer {
        Some((w, n)) if w == me => st.writer = (n > 1).then_some((me, n - 1)),
        _ => return Outcome::Done(false),
      }
      l.inner.set_txn(txn, st);
      Outcome::Done(true)
    })
  }

  /// Total read locks held, across all fibers.
  pub fn read_locks<E: Send + 'static>(&self) -> Stm<usize, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(l.inner.read_txn(txn).readers.values().sum()))
  }

  /// Write locks held (by the single writer, if any).
  pub fn write_locks<E: Send + 'static>(&self) -> Stm<usize, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| Outcome::Done(l.inner.read_txn(txn).writer.map_or(0, |(_, n)| n)))
  }

  /// `true` while some fiber holds the write lock.
  pub fn is_write_locked<E: Send + 'static>(&self) -> Stm<bool, E> {
    self.write_locks().map(|n| n > 0)
  }

  /// `true` while any fiber holds a read or write lock.
  pub fn is_locked<E: Send + 'static>(&self) -> Stm<bool, E> {
    let l = self.clone();
    Stm::from_fn(move |txn| {
      let st = l.inner.read_txn(txn);
      Outcome::Done(st.writer.is_some() || !st.readers.is_empty())
    })
  }
}
