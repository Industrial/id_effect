//! `FiberRef<T>`: fiber-local values inherited by forked children. Filled by `k-scope-fiberref`.
//!
//! Design A §1.3: the values live in the `refs` slots of each [`FiberContext`], never in
//! thread-locals, so they follow the fiber across executor threads. A child forked with
//! [`fork`](crate::next::Effect::fork), `fork_all` or [`Scope::fork_in`](crate::next::Scope)
//! starts with a copy of its parent's slots at fork time; after that, each side's
//! [`set`](FiberRef::set) is invisible to the other. A
//! [`fork_daemon`](crate::next::Effect::fork_daemon) root inherits the same copy (Effect.ts
//! parity); only a fiber started from outside a fiber (`Runtime::fork`, a run edge) starts from
//! the initial values.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::next::kernel::{Effect, Instr, MaskOp, erase};
use crate::next::runtime::{FiberContext, RefSlot};

static NEXT_REF_ID: AtomicU64 = AtomicU64::new(0);

/// A fiber-local variable (Effect.ts `FiberRef`).
///
/// Cloning a `FiberRef` clones the handle, not the value: both handles name the same slot.
/// Reads in a fiber that never set the ref (and did not inherit a value) return the initial
/// value given to [`new`](Self::new).
pub struct FiberRef<T> {
  inner: Arc<RefInner<T>>,
}

struct RefInner<T> {
  id: u64,
  init: T,
}

impl<T> Clone for FiberRef<T> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<T> std::fmt::Debug for FiberRef<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_tuple("FiberRef").field(&self.inner.id).finish()
  }
}

impl<T: Clone + Send + Sync + 'static> FiberRef<T> {
  /// A new fiber ref whose value is `init` in every fiber until it is set.
  pub fn new(init: T) -> Self {
    Self {
      inner: Arc::new(RefInner {
        id: NEXT_REF_ID.fetch_add(1, Ordering::Relaxed),
        init,
      }),
    }
  }

  /// The ref's initial value.
  pub fn initial(&self) -> &T {
    &self.inner.init
  }

  /// The value in `fiber`, outside an effect (a hook for runtime integrations such as metrics
  /// and tracing bridges).
  pub fn get_in(&self, fiber: &FiberContext) -> T {
    match fiber.ref_get(self.inner.id) {
      Some(slot) => match slot.downcast_ref::<T>() {
        Some(v) => v.clone(),
        None => unreachable!("next: fiber ref slot holds a foreign type (kernel bug)"),
      },
      None => self.inner.init.clone(),
    }
  }

  /// Sets the value in `fiber`, outside an effect (see [`get_in`](Self::get_in)).
  pub fn set_in(&self, fiber: &FiberContext, v: T) {
    fiber.ref_set(self.inner.id, Arc::new(v) as RefSlot);
  }

  /// Reads the current fiber's value.
  pub fn get<E: Send + 'static, R: Send + Sync + 'static>(&self) -> Effect<T, E, R> {
    let this = self.clone();
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      Instr::Pure(Ok(erase(this.get_in(fiber))))
    })))
  }

  /// Sets the current fiber's value. Children forked afterwards inherit it; the parent and
  /// already-running children do not see it.
  pub fn set<E: Send + 'static, R: Send + Sync + 'static>(&self, v: T) -> Effect<(), E, R> {
    let this = self.clone();
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      this.set_in(fiber, v);
      Instr::Pure(Ok(erase(())))
    })))
  }

  /// Replaces the current fiber's value with `f(value)`. A panic in `f` becomes `Die` and
  /// leaves the value unchanged.
  pub fn update<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
    f: impl FnOnce(T) -> T + Send + 'static,
  ) -> Effect<(), E, R> {
    let this = self.clone();
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let fiber = Arc::clone(fiber);
      // `f` is a user closure: run it in a `Sync` step so a panic is caught as `Die`.
      Instr::Sync(Box::new(move |_| {
        this.set_in(&fiber, f(this.get_in(&fiber)));
        Instr::Pure(Ok(erase(())))
      }))
    })))
  }

  /// Runs `eff` with the value set to `v`, then restores the previous slot however `eff` ends
  /// (success, failure, defect or interrupt). `eff` keeps the caller's interruptibility, and
  /// children it forks inherit `v`.
  pub fn locally<A, E, R>(&self, v: T, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let id = self.inner.id;
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let m = fiber.mask();
      // Set and restore both happen inside the masked region, so no interrupt can land
      // between them; only `eff` runs with the caller's mask.
      Instr::Mask(
        Box::new(Instr::WithFiber(Box::new(move |_, fiber| {
          let prev = fiber.ref_set(id, Arc::new(v) as RefSlot);
          let fiber = Arc::clone(fiber);
          Instr::Fold(
            Box::new(Instr::Mask(Box::new(eff.into_instr()), MaskOp::Set(m))),
            Box::new(move |exit| {
              fiber.ref_restore(id, prev);
              Instr::Pure(exit)
            }),
          )
        }))),
        MaskOp::Inc,
      )
    })))
  }
}
