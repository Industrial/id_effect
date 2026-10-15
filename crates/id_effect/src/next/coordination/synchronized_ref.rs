//! [`SynchronizedRef`]: a [`Ref`] whose effectful updates are serialized (Effect.ts
//! `SynchronizedRef`). Ported from the v1 `coordination::synchronized_ref`, which held a tokio
//! mutex across the inner effect.
//!
//! Every operation runs under a one-permit [`Semaphore`], held until the inner effect completes
//! and released however it ends (interrupt included), so concurrent callers never interleave.

use super::ref_::Ref;
use super::semaphore::Semaphore;
use crate::next::kernel::Effect;

/// A shared cell with strictly serialized (effectful) updates. Cloning yields another handle to
/// the same cell.
pub struct SynchronizedRef<A> {
  cell: Ref<A>,
  gate: Semaphore,
}

impl<A> Clone for SynchronizedRef<A> {
  fn clone(&self) -> Self {
    Self {
      cell: self.cell.clone(),
      gate: self.gate.clone(),
    }
  }
}

impl<A: std::fmt::Debug> std::fmt::Debug for SynchronizedRef<A> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("SynchronizedRef").field(&self.cell).finish()
  }
}

impl<A: Clone + Send + 'static> SynchronizedRef<A> {
  /// A new cell holding `initial`.
  pub fn new(initial: A) -> Self {
    Self {
      cell: Ref::new(initial),
      gate: Semaphore::new(1),
    }
  }

  fn serialized<B, E, R>(&self, eff: Effect<B, E, R>) -> Effect<B, E, R>
  where
    B: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.gate.with_permit(eff)
  }

  /// Reads the current value (after any update in progress).
  pub fn get<E, R>(&self) -> Effect<A, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.serialized(self.cell.get())
  }

  /// Replaces the value.
  pub fn set<E, R>(&self, value: A) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.serialized(self.cell.set(value))
  }

  /// Replaces the value with `f(current)`.
  pub fn update<E, R>(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.serialized(self.cell.update(f))
  }

  /// Computes `(b, new)` from the current value, stores `new` and returns `b`.
  pub fn modify<B, E, R>(&self, f: impl FnOnce(A) -> (B, A) + Send + 'static) -> Effect<B, E, R>
  where
    B: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.serialized(self.cell.modify(f))
  }

  /// Runs `f`'s effect on the current value and stores its result; the cell stays locked until
  /// the effect completes. On failure the cell is unchanged.
  pub fn update_effect<E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<A, E, R> + Send + 'static,
  ) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let cell = self.cell.clone();
    let set = self.cell.clone();
    self.serialized(cell.get().flat_map(f).flat_map(move |a| set.set(a)))
  }

  /// Runs `f`'s effect on the current value; stores the new value it yields and returns its `b`.
  /// The cell stays locked until the effect completes. On failure the cell is unchanged.
  pub fn modify_effect<B, E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<(B, A), E, R> + Send + 'static,
  ) -> Effect<B, E, R>
  where
    B: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let cell = self.cell.clone();
    let set = self.cell.clone();
    self.serialized(
      cell
        .get()
        .flat_map(f)
        .flat_map(move |(b, a)| set.set(a).map(move |()| b)),
    )
  }

  /// Runs `f`'s effect on the current value and returns its result, leaving the cell unchanged;
  /// the cell stays locked until the effect completes.
  pub fn get_effect<E, R>(
    &self,
    f: impl FnOnce(A) -> Effect<A, E, R> + Send + 'static,
  ) -> Effect<A, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.serialized(self.cell.get().flat_map(f))
  }
}
