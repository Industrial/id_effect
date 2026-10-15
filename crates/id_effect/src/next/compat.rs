//! The temporary v1 ↔ `next` bridge for crates that have not switched yet (release plan §3b).
//!
//! Every item is `#[doc(hidden)]`. Reached as `id_effect::next::compat::*` only (not glob
//! re-exported from `next`), so `k-retire-v1` can assert `rg 'next::compat'` = 0 and delete it.
//! Import the traits and use the method forms:
//!
//! ```
//! use id_effect::next::compat::{IntoNext, IntoV1};
//! use id_effect::next::{self, HNil};
//!
//! // next → v1: an unmigrated caller runs a migrated API under its own v1 env (`()` here).
//! let v1 = next::sync::<u32, String, HNil>(|| 20).into_v1::<()>();
//! assert_eq!(id_effect::run_blocking(v1.map(|n| n + 1), ()), Ok(21));
//!
//! // v1 → next: a migrated crate drives a v1 effect it was handed.
//! let local = id_effect::succeed::<u32, String, ()>(1).into_next();
//! assert!(matches!(local.block_on(), id_effect::Exit::Success(1)));
//! ```
//!
//! # `IntoV1`: `next::Effect` → v1 `Effect`
//!
//! [`IntoV1::into_v1`] turns a `next::Effect<A, E, HNil>` into a lazy v1 `Effect<A, E, R>` for
//! **any** v1 env `R`: the next effect needs nothing, so the v1 env is ignored. When the v1
//! effect runs, the next effect is forked as a root fiber on the `next` runtime
//! ([`Runtime::global`], or the one given to [`IntoV1::into_v1_on`]) and the v1 future awaits its
//! exit. A ready (`Pure`) next effect completes inline, without a fiber.
//!
//! Outcome mapping (`Cause` → v1 `Result`):
//!
//! | next exit | v1 result |
//! |---|---|
//! | `Success(a)` | `Ok(a)` |
//! | `Fail(e)` (the first typed failure of a composite cause) | `Err(e)` |
//! | `Die(d)` (no typed failure) | a panic: `resume_unwind` of `d`'s payload, or of its message |
//! | `Interrupt(id)` (no typed failure, no defect) | a panic carrying the kernel's interrupt signal |
//!
//! v1 has no defect or interrupt channel, so those become panics, the v1 notion of a defect.
//! [`IntoNext`] turns both panics back into `Die` / `Interrupt`, so a round trip
//! next → v1 → next keeps the cause (and a panic payload). For a lossless v1 view use
//! [`IntoV1::into_v1_exit`] (`Ok(Exit<A, E>)`, never fails) or [`IntoV1::into_v1_cause`]
//! (`Err(Cause<E>)`).
//!
//! **Cancellation.** v1 cancels by dropping the future. Dropping the v1 future before the next
//! fiber is done requests the fiber's interruption, so its finalizers (`ensuring`,
//! `acquire_use_release`) run on the next runtime. The drop does not wait for them.
//!
//! # `IntoNext`: v1 `Effect` → `next::LocalEffect`
//!
//! A v1 `Effect` is `!Send`, so it becomes a [`LocalEffect`]: run it with `block_on`, bind it with
//! `~` in `effect_local_next!`, drive it with `run_on`, or start it on an executor's
//! `spawn_local` worker with `run_on_local` (which hands back a `Send` next `Effect`). The v1
//! future runs as one async step of the local runloop:
//!
//! - `Ok(a)` → `Success(a)`, `Err(e)` → `Fail(e)`;
//! - a panic → `Die` (payload kept), or `Interrupt` for a panic raised by [`IntoV1`];
//! - interrupting the next fiber drops the v1 future (v1 cancellation).
//!
//! # Environment mapping
//!
//! v1 threads one owned env value by `&mut R`; next has a typed capability list (`HNil`,
//! `HCons<T, …>` of `Arc`-shared `Send + Sync` values). The bridge never guesses between them:
//!
//! | direction | method | env |
//! |---|---|---|
//! | next → v1 | [`into_v1`](IntoV1::into_v1) | next `HNil`; any v1 `R`, ignored |
//! | next → v1 | [`into_v1_with`](IntoV1With::into_v1_with) | next `Rn` built from the v1 `&mut R` by a closure at run time (`provide_all`) |
//! | v1 → next | [`into_next`](IntoNext::into_next) | v1 `()`; next `HNil` |
//! | v1 → next | [`into_next_in`](IntoNext::into_next_in) | v1 `()`; any next `Rn` (to bind inside a typed-`R` program) |
//! | v1 → next | [`into_next_with`](IntoNext::into_next_with) | v1 `R` given by value; next `HNil` |
//! | v1 → next | [`into_next_from`](IntoNext::into_next_from) | v1 `R: Clone`, cloned out of the next capability `R` in `Rn` |
//!
//! A next effect that needs capabilities can also be `provide`d first and then use
//! [`into_v1`](IntoV1::into_v1).

use std::future::Future;
use std::panic::resume_unwind;
use std::sync::Arc;

use crate::failure::{Cause, Defect, Exit};
use crate::kernel::{BoxFuture as V1BoxFuture, Effect as V1Effect, box_future};
use crate::next::capability::{HNil, Selector};
use crate::next::kernel::{Ctx, Effect, InterruptSignal, LocalEffect, Node, split_failure};
use crate::next::runtime::{Executor, FiberContext, Runtime};
use crate::runtime::{FiberId, Never};

#[doc(hidden)]
mod sealed {
  /// The v1 envs [`IntoNext::into_next`](super::IntoNext::into_next) accepts: `()` only.
  pub trait UnitEnv {
    /// The env value.
    fn unit() -> Self;
  }

  impl UnitEnv for () {
    fn unit() -> Self {}
  }
}

#[doc(hidden)]
pub use sealed::UnitEnv;

/// Requests the next fiber's interruption when the v1 future is dropped before its exit.
struct InterruptOnDrop(Option<Arc<FiberContext>>);

impl Drop for InterruptOnDrop {
  fn drop(&mut self) {
    if let Some(fiber) = self.0.take() {
      fiber.request_interrupt();
    }
  }
}

/// Runs `eff` as a root fiber on `exec` (or inline when ready) and awaits its exit. Dropping the
/// future before the exit interrupts the fiber.
fn await_next<A, E>(
  eff: Effect<A, E, HNil>,
  exec: Arc<dyn Executor>,
) -> impl Future<Output = Exit<A, E>>
where
  A: Send + 'static,
  E: Send + 'static,
{
  let ready = match eff.node {
    Node::Pure(r) => Ok(r),
    node => Err(Effect { node }),
  };
  async move {
    match ready {
      Ok(Ok(a)) => Exit::Success(a),
      Ok(Err(c)) => Exit::Failure(c),
      Err(eff) => {
        let handle = eff.fork_root(exec);
        let mut guard = InterruptOnDrop(Some(Arc::clone(handle.fiber())));
        let exit = handle.join_future().await;
        guard.0 = None;
        exit
      }
    }
  }
}

/// Re-raises a cause without a typed failure as a v1 defect (a panic): the first `Die` leaf
/// (its payload, else its message), otherwise the first `Interrupt` as the kernel's interrupt
/// signal, which [`IntoNext`] turns back into `Cause::Interrupt`.
fn raise(cause: Cause<Never>) -> ! {
  fn first_die(c: &Cause<Never>) -> Option<&Defect> {
    match c {
      Cause::Die(d) => Some(d),
      Cause::Both(l, r) | Cause::Then(l, r) => first_die(l).or_else(|| first_die(r)),
      Cause::Fail(n) => match *n {},
      Cause::Interrupt(_) => None,
    }
  }
  fn first_interrupt(c: &Cause<Never>) -> Option<FiberId> {
    match c {
      Cause::Interrupt(id) => Some(*id),
      Cause::Both(l, r) | Cause::Then(l, r) => first_interrupt(l).or_else(|| first_interrupt(r)),
      Cause::Fail(n) => match *n {},
      Cause::Die(_) => None,
    }
  }
  if let Some(d) = first_die(&cause) {
    match d.take_payload() {
      Some(payload) => resume_unwind(payload),
      None => resume_unwind(Box::new(d.message().to_string())),
    }
  }
  match first_interrupt(&cause) {
    Some(id) => resume_unwind(Box::new(InterruptSignal(id))),
    None => resume_unwind(Box::new(format!(
      "next::compat: a cause with no leaf: {cause:?}"
    ))),
  }
}

/// The v1 result of a next exit (see the module docs' outcome table).
fn v1_result<A, E>(exit: Exit<A, E>) -> Result<A, E> {
  match exit {
    Exit::Success(a) => Ok(a),
    Exit::Failure(c) => match split_failure::<E, Never>(c) {
      Ok(e) => Err(e),
      Err(c) => raise(c),
    },
  }
}

/// `next::Effect<A, E, HNil>` → v1 `Effect` (release plan §3b, the upward bridge). See the
/// module docs for the outcome and cancellation mapping.
#[doc(hidden)]
pub trait IntoV1<A: 'static, E: 'static>: Sized {
  /// A v1 effect (any env `R`, ignored) that runs `self` on [`Runtime::global`].
  fn into_v1<R: 'static>(self) -> V1Effect<A, E, R>;

  /// [`into_v1`](Self::into_v1) on `rt`'s executor.
  fn into_v1_on<R: 'static>(self, rt: &Runtime) -> V1Effect<A, E, R>;

  /// Lossless: the whole next [`Exit`] as the v1 success value (never fails).
  fn into_v1_exit<R: 'static, E2: 'static>(self) -> V1Effect<Exit<A, E>, E2, R>;

  /// Lossless on the error side: the whole next [`Cause`] as the v1 error.
  fn into_v1_cause<R: 'static>(self) -> V1Effect<A, Cause<E>, R>;
}

impl<A, E> IntoV1<A, E> for Effect<A, E, HNil>
where
  A: Send + 'static,
  E: Send + 'static,
{
  fn into_v1<R: 'static>(self) -> V1Effect<A, E, R> {
    self.into_v1_on(Runtime::global())
  }

  fn into_v1_on<R: 'static>(self, rt: &Runtime) -> V1Effect<A, E, R> {
    let exec = Arc::clone(rt.executor());
    V1Effect::new_async(move |_: &mut R| -> V1BoxFuture<'_, Result<A, E>> {
      box_future(async move { v1_result(await_next(self, exec).await) })
    })
  }

  fn into_v1_exit<R: 'static, E2: 'static>(self) -> V1Effect<Exit<A, E>, E2, R> {
    let exec = Arc::clone(Runtime::global().executor());
    V1Effect::new_async(
      move |_: &mut R| -> V1BoxFuture<'_, Result<Exit<A, E>, E2>> {
        box_future(async move { Ok(await_next(self, exec).await) })
      },
    )
  }

  fn into_v1_cause<R: 'static>(self) -> V1Effect<A, Cause<E>, R> {
    let exec = Arc::clone(Runtime::global().executor());
    V1Effect::new_async(move |_: &mut R| -> V1BoxFuture<'_, Result<A, Cause<E>>> {
      box_future(async move {
        match await_next(self, exec).await {
          Exit::Success(a) => Ok(a),
          Exit::Failure(c) => Err(c),
        }
      })
    })
  }
}

/// `next::Effect<A, E, Rn>` with capabilities → v1 `Effect<A, E, R>`, building the next env from
/// the v1 env.
#[doc(hidden)]
pub trait IntoV1With<A: 'static, E: 'static, Rn>: Sized {
  /// A v1 effect that, when run, builds the next env with `f(&mut r)`, provides it
  /// (`provide_all`) and runs as [`IntoV1::into_v1`] does.
  fn into_v1_with<R: 'static>(self, f: impl FnOnce(&mut R) -> Rn + 'static) -> V1Effect<A, E, R>;
}

impl<A, E, Rn> IntoV1With<A, E, Rn> for Effect<A, E, Rn>
where
  A: Send + 'static,
  E: Send + 'static,
  Rn: Send + Sync + 'static,
{
  fn into_v1_with<R: 'static>(self, f: impl FnOnce(&mut R) -> Rn + 'static) -> V1Effect<A, E, R> {
    let exec = Arc::clone(Runtime::global().executor());
    V1Effect::new_async(move |r: &mut R| -> V1BoxFuture<'_, Result<A, E>> {
      let eff = self.provide_all(f(r));
      box_future(async move { v1_result(await_next(eff, exec).await) })
    })
  }
}

/// v1 `Effect<A, E, R>` → `next::LocalEffect` (release plan §3b). See the module docs for the
/// outcome and env mapping.
#[doc(hidden)]
pub trait IntoNext<A, E, R>: Sized {
  /// A v1 effect over `()` as a `LocalEffect<A, E, HNil>`.
  fn into_next(self) -> LocalEffect<A, E, HNil>
  where
    R: UnitEnv;

  /// A v1 effect over `()` as a `LocalEffect` under any next env `Rn` (it needs nothing from it).
  fn into_next_in<Rn: Send + Sync + 'static>(self) -> LocalEffect<A, E, Rn>
  where
    R: UnitEnv;

  /// Runs the v1 effect against the owned env `env`.
  fn into_next_with(self, env: R) -> LocalEffect<A, E, HNil>;

  /// Runs the v1 effect against a clone of the next capability `R` in `Rn` (index `I` inferred).
  fn into_next_from<Rn, I>(self) -> LocalEffect<A, E, Rn>
  where
    R: Clone + Send + Sync,
    Rn: Selector<R, I> + Send + Sync + 'static;
}

/// One v1 run against an owned env.
async fn run_v1<A: 'static, E: 'static, R: 'static>(
  eff: V1Effect<A, E, R>,
  mut env: R,
) -> Result<A, E> {
  eff.run(&mut env).await
}

impl<A, E, R> IntoNext<A, E, R> for V1Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  fn into_next(self) -> LocalEffect<A, E, HNil>
  where
    R: UnitEnv,
  {
    self.into_next_in()
  }

  fn into_next_in<Rn: Send + Sync + 'static>(self) -> LocalEffect<A, E, Rn>
  where
    R: UnitEnv,
  {
    LocalEffect::from_future(move || run_v1(self, R::unit()))
  }

  fn into_next_with(self, env: R) -> LocalEffect<A, E, HNil> {
    LocalEffect::from_future(move || run_v1(self, env))
  }

  fn into_next_from<Rn, I>(self) -> LocalEffect<A, E, Rn>
  where
    R: Clone + Send + Sync,
    Rn: Selector<R, I> + Send + Sync + 'static,
  {
    LocalEffect::from_ctx(move |ctx: Ctx<Rn>| {
      let env = R::clone(Selector::<R, I>::get(ctx.env()));
      run_v1(self, env)
    })
  }
}
