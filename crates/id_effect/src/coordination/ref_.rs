//! [`Ref`]: a shared mutable cell (Effect.ts `Ref<A>`). Ported from the v1
//! `coordination::ref_`.
//!
//! Every operation is a lazy synchronous step that holds the cell's mutex only while it runs;
//! nothing is held across a suspension. [`Ref::modify_effect`] reads a snapshot, runs the inner
//! effect without the lock and stores the result, so concurrent `modify_effect` calls interleave;
//! use [`SynchronizedRef`](crate::SynchronizedRef) for strictly serialized effectful
//! updates.

use std::sync::{Arc, Mutex};

use super::waiters::lock;
use crate::kernel::{Effect, suspend, sync};

/// A shared mutable cell. Cloning yields a second handle to the **same** cell.
///
/// The constructor is a plain function; every operation returns a lazy [`Effect`] that is
/// generic over its error type `E` and environment `R` (it never fails and needs nothing).
///
/// ```
/// use id_effect::Ref;
/// use id_effect::Never;
///
/// let r = Ref::new(1_u32);
/// let eff = r.update::<Never, _>(|n| n + 1).then(r.get());
/// assert_eq!(eff.block_on().into_result().ok(), Some(2));
/// ```
pub struct Ref<A> {
  cell: Arc<Mutex<A>>,
}

impl<A> Clone for Ref<A> {
  fn clone(&self) -> Self {
    Self {
      cell: Arc::clone(&self.cell),
    }
  }
}

impl<A: std::fmt::Debug> std::fmt::Debug for Ref<A> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("Ref").field(&*lock(&self.cell)).finish()
  }
}

impl<A: Send + 'static> Ref<A> {
  /// A new cell holding `value`.
  pub fn new(value: A) -> Self {
    Self {
      cell: Arc::new(Mutex::new(value)),
    }
  }

  /// Reads the current value.
  pub fn get<E, R>(&self) -> Effect<A, E, R>
  where
    A: Clone,
  {
    let cell = Arc::clone(&self.cell);
    sync(move || lock(&cell).clone())
  }

  /// Replaces the value.
  pub fn set<E, R>(&self, value: A) -> Effect<(), E, R> {
    let cell = Arc::clone(&self.cell);
    sync(move || *lock(&cell) = value)
  }

  /// Replaces the value with `f(current)`.
  pub fn update<E, R>(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<(), E, R>
  where
    A: Clone,
  {
    let cell = Arc::clone(&self.cell);
    sync(move || {
      let mut g = lock(&cell);
      *g = f(g.clone());
    })
  }

  /// Replaces the value with `f(current)` and returns the **new** value.
  pub fn update_and_get<E, R>(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<A, E, R>
  where
    A: Clone,
  {
    let cell = Arc::clone(&self.cell);
    sync(move || {
      let mut g = lock(&cell);
      *g = f(g.clone());
      g.clone()
    })
  }

  /// Replaces the value with `f(current)` and returns the **old** value.
  pub fn get_and_update<E, R>(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<A, E, R>
  where
    A: Clone,
  {
    let cell = Arc::clone(&self.cell);
    sync(move || {
      let mut g = lock(&cell);
      let old = g.clone();
      *g = f(old.clone());
      old
    })
  }

  /// Atomically computes `(b, new)` from the current value, stores `new` and returns `b`.
  pub fn modify<B, E, R>(&self, f: impl FnOnce(A) -> (B, A) + Send + 'static) -> Effect<B, E, R>
  where
    A: Clone,
  {
    let cell = Arc::clone(&self.cell);
    sync(move || {
      let mut g = lock(&cell);
      let (b, new) = f(g.clone());
      *g = new;
      b
    })
  }

  /// Stores `value` and returns the **old** value.
  pub fn get_and_set<E, R>(&self, value: A) -> Effect<A, E, R> {
    let cell = Arc::clone(&self.cell);
    sync(move || std::mem::replace(&mut *lock(&cell), value))
  }

  /// Stores `value` and returns it.
  pub fn set_and_get<E, R>(&self, value: A) -> Effect<A, E, R>
  where
    A: Clone,
  {
    let cell = Arc::clone(&self.cell);
    sync(move || {
      *lock(&cell) = value.clone();
      value
    })
  }

  /// Runs `f`'s effect on a snapshot of the current value and stores the new value it yields;
  /// returns its `b`. When the inner effect fails, the cell is left unchanged.
  ///
  /// The lock is **not** held while the inner effect runs, so concurrent calls interleave (last
  /// writer wins). Use [`SynchronizedRef`](crate::SynchronizedRef) to serialize them.
  pub fn modify_effect<B, E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<(B, A), E, R> + Send + 'static,
  ) -> Effect<B, E, R>
  where
    A: Clone,
    B: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let cell = Arc::clone(&self.cell);
    suspend(move || {
      let old = lock(&cell).clone();
      f(old).map(move |(b, new)| {
        *lock(&cell) = new;
        b
      })
    })
  }
}
