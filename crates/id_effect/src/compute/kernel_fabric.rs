//! The Compute Fabric as the fiber admission scheduler (ADR 0007 Amendment 1, D6). Filled by
//! `c-fabric-scheduler`.
//!
//! [`Concurrency::Inherit`](crate::Concurrency::Inherit) awaits an asynchronous admission
//! permit from the ambient [`Fabric`] before each child of
//! [`fork_all`](crate::fork_all) (and so of `all`, `for_each_par`, `zip_par`, …) starts.
//! [`Concurrency::Bounded(n)`](crate::Concurrency::Bounded) stays a purely local bound
//! and never consults the fabric; with no ambient fabric, `Inherit` is `Unbounded`.
//!
//! # Finding the ambient fabric
//!
//! Effect.ts parity: `concurrency: "inherit"` reads the `currentConcurrency` `FiberRef`, set by
//! `Effect.withConcurrency`. Here the ambient fabric is likewise a fiber-local value — a private
//! [`FiberRef`] set by [`Fabric::provide`] — not a capability in `R`, so any effect can use
//! `Inherit` whatever its environment, and children inherit the fabric at fork time. The
//! process-wide v1 fabric (`id_effect::compute::install_fabric`) is not consulted.
//!
//! # Admission
//!
//! A child admitted through the fabric holds its [`AdmissionPermit`] for as long as it runs;
//! the permit is dropped when the child exits (success, failure, defect or interrupt). Waiting
//! is a suspended future with a registered waker (no thread is held, no blocking acquisition),
//! and it is interruptible: interrupting the forking fiber drops the pending admission, which
//! takes nothing, so no permit leaks.
//!
//! Nested `Inherit` forks do not deadlock: a fiber that holds a permit of the ambient fabric
//! only waits while its own children run, so it lends its permit to one child at a time; that
//! child starts without a permit of its own, further siblings await fresh permits. Every
//! admitted subtree can therefore always make progress, however small the capacity.
//!
//! ```
//! use std::sync::Arc;
//! use id_effect::{Concurrency, Effect, Fabric, TestExecutor, all, succeed};
//! use id_effect::Never;
//!
//! let fabric = Fabric::with_capacity(2); // at most 2 `Inherit` children run at once
//! let work: Vec<Effect<u32, Never>> = (0..8).map(succeed).collect();
//! let eff = fabric.provide(all(work, Concurrency::Inherit));
//!
//! let exec = TestExecutor::new();
//! let h = eff.fork_root(Arc::new(exec.clone()));
//! exec.run_until_stalled();
//! assert!(h.is_done());
//! ```

use std::fmt;
use std::sync::{Arc, OnceLock};

#[cfg(feature = "compute")]
use crate::compute::{AdmissionController, ComputeFabric, TelemetryEngine};
use crate::concurrency::{Admission, AdmissionPermit};
use crate::kernel::Effect;
use crate::runtime::{BoxFuture, FiberContext, FiberRef};

/// The compute fabric as seen by the 0.7 kernel: the [`Admission`] that
/// [`Concurrency::Inherit`](crate::Concurrency::Inherit) children await.
///
/// Cheap to clone (an `Arc`); clones share the same capacity.
#[derive(Clone)]
pub struct Fabric {
  admission: Arc<dyn Admission>,
}

impl fmt::Debug for Fabric {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Fabric").finish_non_exhaustive()
  }
}

impl Fabric {
  /// A fabric admitting through `admission`.
  pub fn new(admission: impl Admission + 'static) -> Self {
    Self {
      admission: Arc::new(admission),
    }
  }

  /// A fabric with a fixed capacity of `capacity` concurrently admitted children (`0` counts
  /// as `1`), backed by an [`AdmissionController`].
  #[cfg(feature = "compute")]
  pub fn with_capacity(capacity: usize) -> Self {
    let n = capacity.max(1);
    Self::from_controller(Arc::new(AdmissionController::new(n, n)))
  }

  /// A fabric admitting through `controller`'s async
  /// [`admit_n`](AdmissionController::admit_n) (a weight of `w` takes `w` permits). Its
  /// capacity follows the controller, so a governor's `rebalance` resizes it live.
  #[cfg(feature = "compute")]
  pub fn from_controller(controller: Arc<AdmissionController>) -> Self {
    Self::new(Controller(controller))
  }

  /// A fabric admitting through a v1 [`ComputeFabric`]'s async
  /// [`admit`](ComputeFabric::admit) (one permit per admission), so its `ResourcePolicy`
  /// governor throttles `Inherit` children.
  #[cfg(feature = "compute")]
  pub fn from_compute<E: TelemetryEngine + 'static>(fabric: Arc<ComputeFabric<E>>) -> Self {
    Self::new(Compute(fabric))
  }

  /// Awaits admission for work of `weight` units (see [`Admission::admit`]).
  pub fn admit(&self, weight: u32) -> BoxFuture<'static, AdmissionPermit> {
    self.admission.admit(weight)
  }

  /// Runs `eff` with `self` as the ambient fabric (Effect.ts `Effect.withConcurrency`): its
  /// `Inherit` forks, and those of the children it forks, are admitted through `self`. The
  /// previous ambient fabric is restored however `eff` ends.
  pub fn provide<A, E, R>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let ambient = Ambient {
      fabric: self.clone(),
      admitted: false,
    };
    ambient_ref().locally(Some(ambient), eff)
  }

  /// The current fiber's ambient fabric, if any.
  pub fn current<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<Option<Fabric>, E, R> {
    ambient_ref()
      .get()
      .map(|a: Option<Ambient>| a.map(|a| a.fabric))
  }

  /// The ambient fabric of `fiber`, outside an effect (a hook for runtime integrations).
  pub fn current_in(fiber: &FiberContext) -> Option<Fabric> {
    ambient_ref().get_in(fiber).map(|a| a.fabric)
  }
}

/// The ambient-fabric slot of a fiber.
#[derive(Clone)]
pub(crate) struct Ambient {
  pub(crate) fabric: Fabric,
  /// This fiber holds a permit of `fabric` (it was admitted through it, or runs on a permit its
  /// parent lent it), so its own `Inherit` forks may lend that permit to one child at a time.
  pub(crate) admitted: bool,
}

impl Ambient {
  /// `fiber`'s ambient fabric slot.
  pub(crate) fn of(fiber: &FiberContext) -> Option<Ambient> {
    ambient_ref().get_in(fiber)
  }

  /// Records in the not-yet-started child `ctx` that it holds a permit of `fabric`.
  pub(crate) fn mark_admitted(ctx: &FiberContext, fabric: &Fabric) {
    let ambient = Ambient {
      fabric: fabric.clone(),
      admitted: true,
    };
    ambient_ref().set_in(ctx, Some(ambient));
  }
}

fn ambient_ref() -> &'static FiberRef<Option<Ambient>> {
  static AMBIENT: OnceLock<FiberRef<Option<Ambient>>> = OnceLock::new();
  AMBIENT.get_or_init(|| FiberRef::new(None))
}

#[cfg(feature = "compute")]
struct Controller(Arc<AdmissionController>);

#[cfg(feature = "compute")]
impl Admission for Controller {
  fn admit(&self, weight: u32) -> BoxFuture<'static, AdmissionPermit> {
    let admit = self.0.admit_n(weight as usize);
    Box::pin(async move { AdmissionPermit::new(admit.await) })
  }
}

#[cfg(feature = "compute")]
struct Compute<E: TelemetryEngine>(Arc<ComputeFabric<E>>);

#[cfg(feature = "compute")]
impl<E: TelemetryEngine + 'static> Admission for Compute<E> {
  fn admit(&self, _weight: u32) -> BoxFuture<'static, AdmissionPermit> {
    let admit = self.0.admit();
    Box::pin(async move { AdmissionPermit::new(admit.await) })
  }
}
