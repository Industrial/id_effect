//! Support items for the `effect!` / `effect_local!` / `caps!` expansions, reached as
//! `id_effect::__private`. Not API.
//!
//! - [`Lift`] with [`LiftNever`] / [`LiftFrom`]: error widening for `~` binds by autoref
//!   specialisation (spike A `lib.rs:46-84`). `(&&Lift::new(e)).lift()` picks `LiftNever` for a
//!   `Never` error (widens to anything) and `LiftFrom` otherwise (`E: From<E2>`).
//! - [`bind_local`]: what `~expr` expands to inside `effect_local!`.
//! - [`CapsChecked`]: the identity projection `caps!` wraps a de-duplicated list in when it
//!   also reports a duplicate (so the error does not cascade).
//! - [`branch`] and [`join_binds`]: what a parallel join `~(a, b, ..)` (2 to 8 effects) expands
//!   to inside `effect!`.
//!
//! # Parallel joins: `~(a, b, ..)`
//!
//! A `~` followed by a parenthesised, comma-separated list of 2 to 8 effects runs them as child
//! fibers through [`all`](crate::all) (one `fork_all`, `Concurrency::Unbounded`) and yields
//! the tuple of their values, in input order. It shares `zip_par`'s semantics: the first failure
//! interrupts the siblings still running and the join waits for them (their finalizers run)
//! before it fails; interrupting the body's fiber interrupts every branch.
//!
//! The body sees the **first** failure, the leftmost leaf of `zip_par`'s cause: its typed error
//! as the `~`'s error (widened like any `~` error, per branch), or its defect or interrupt as the
//! body's cause. Genuine sibling failures that `zip_par` adds with `Cause::Both` are dropped (one
//! `?` carries one error); bind `~zip_par(a, b).exit::<Never>()` to keep the whole cause as data.
//!
//! Each branch is an effect over a part of the body's environment (narrowed, as `~` does) or a
//! `.same_env()` one. Binds are statements, so a branch may not refer to another branch's value;
//! independent `let a = ~x; let b = ~y;` statements stay sequential (no implicit parallelism).
//!
//! ```
//! use id_effect::{Effect, effect, fail, succeed};
//! use id_effect::{Cause, Exit};
//!
//! fn both() -> Effect<(u8, &'static str), String> {
//!   effect!({ ~(succeed::<u8, String, _>(1), succeed::<_, String, _>("two")) })
//! }
//! assert!(matches!(both().block_on(), Exit::Success((1, "two"))));
//!
//! fn failing() -> Effect<u8, String> {
//!   effect!({
//!     let (a, _b) = ~(succeed::<u8, String, _>(1), fail::<u8, &'static str, _>("boom"));
//!     a
//!   })
//! }
//! assert!(matches!(failing().block_on(), Exit::Failure(Cause::Fail(e)) if e == "boom"));
//! ```
//!
//! More than 8 branches is a compile error (nest a join, or use `all`):
//!
//! ```compile_fail
//! use id_effect::{Effect, effect, succeed};
//! use id_effect::Never;
//!
//! fn one() -> Effect<u8, Never> {
//!   succeed(1)
//! }
//! fn nine() -> Effect<u8, Never> {
//!   effect!({
//!     let (a, ..) = ~(one(), one(), one(), one(), one(), one(), one(), one(), one());
//!     a
//!   })
//! }
//! ```
//!
//! So is a join in `effect_local!`:
//!
//! ```compile_fail
//! use id_effect::{Effect, LocalEffect, effect_local, succeed};
//! use id_effect::Never;
//!
//! fn one() -> Effect<u8, Never> {
//!   succeed(1)
//! }
//! fn local() -> LocalEffect<u8, Never> {
//!   effect_local!({
//!     let (a, b) = ~(one(), one());
//!     a + b
//!   })
//! }
//! ```

use std::any::Any;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::anybox::AnyEnv;
use super::bind::{BindFuture, surface, surface_first};
use super::bind_same::{SameEnv, SameEnvIndex};
use super::cause::from_exit;
use super::ctx::Ctx;
use super::effect::Effect;
use super::local::{HoldMask, IntoLocal};
use crate::capability::{Embed, HList};
use crate::concurrency::Concurrency;
use crate::failure::Never;
use crate::par::all;

/// A `~` bind's error, waiting to be widened into the body's error type.
pub struct Lift<E>(Cell<Option<E>>);

impl<E> Lift<E> {
  /// Wraps the sub-effect's error.
  pub fn new(e: E) -> Self {
    Self(Cell::new(Some(e)))
  }
}

/// Widening of a `Never` error (picked first by autoref).
pub trait LiftNever {
  /// Any `E` (a `Never` has no values).
  fn lift<E>(&self) -> E;
}

impl LiftNever for &Lift<Never> {
  fn lift<E>(&self) -> E {
    match self.0.take() {
      Some(n) => match n {},
      None => unreachable!("id_effect: `~` error lifted twice (macro bug)"),
    }
  }
}

/// Widening through `From` (every other error type).
pub trait LiftFrom<E2> {
  /// `E::from(e2)`.
  fn lift<E: From<E2>>(&self) -> E;
}

impl<E2> LiftFrom<E2> for Lift<E2> {
  fn lift<E: From<E2>>(&self) -> E {
    match self.0.take() {
      Some(e) => E::from(e),
      None => unreachable!("id_effect: `~` error lifted twice (macro bug)"),
    }
  }
}

/// `~sub` inside `effect_local!`: runs a [`LocalEffect`](super::LocalEffect) or a `Send`
/// [`Effect`](super::Effect) (via [`IntoLocal`]) whose environment `Sub` is a part of `R`, on
/// `ctx`'s fiber, as a nested local runloop. Outcomes surface as in [`Ctx::bind`]: the typed error
/// as `Err`, a defect or an interrupt re-raised in the enclosing runloop.
pub fn bind_local<R, X, A, E, Sub, Is>(
  ctx: &Ctx<R>,
  sub: X,
) -> impl Future<Output = Result<A, E>> + use<R, X, A, E, Sub, Is>
where
  X: IntoLocal<A, E, Sub>,
  R: Embed<Sub, Is> + Send + Sync + 'static,
  Sub: Send + Sync + 'static,
  A: 'static,
  E: 'static,
{
  // Same environment: share it. Otherwise project the subset (one `Arc` clone per cap).
  let env: Arc<Sub> = match (Arc::clone(&ctx.env) as AnyEnv).downcast::<Sub>() {
    Ok(same) => same,
    Err(_) => Arc::new(Embed::<Sub, Is>::project(&*ctx.env)),
  };
  let run = sub
    .into_local()
    .run_in(Ctx::new(env, Arc::clone(&ctx.fiber)));
  // Holds a mask level while suspended, as the `Send` bind does (`BindFuture`), so an interrupt
  // reaches the nested runloop and its finalizers run.
  let run = HoldMask::new(run, Arc::clone(&ctx.fiber));
  async move { surface(from_exit(run.await)) }
}

/// The identity on `Self`, with a const argument that carries `caps!`'s duplicate errors.
pub trait CapsChecked<const N: usize> {
  /// `Self`.
  type Out;
}

impl<T, const N: usize> CapsChecked<N> for T {
  type Out = T;
}

/// One branch of a `~(a, b, ..)` join, as an effect over the body's environment `R`.
#[diagnostic::on_unimplemented(
  message = "`{Self}` cannot be a branch of a `~(..)` join in an effect over `{R}`",
  label = "not an effect over a part of `{R}`",
  note = "each branch of `~(a, b, ..)` is an `Effect` whose capabilities are all in the enclosing effect's environment"
)]
pub trait JoinBranch<R, Is> {
  /// The branch's success type.
  type Output;
  /// The branch's typed error.
  type Error;
  /// The branch over `R`.
  fn into_branch(self) -> Effect<Self::Output, Self::Error, R>;
}

impl<B, E2, Sub, R, Is> JoinBranch<R, Is> for Effect<B, E2, Sub>
where
  B: Send + 'static,
  E2: Send + 'static,
  Sub: HList + Send + Sync + 'static,
  R: Embed<Sub, Is> + HList,
{
  type Output = B;
  type Error = E2;

  fn into_branch(self) -> Effect<B, E2, R> {
    self.narrow::<R, Is>()
  }
}

impl<B, E2, R> JoinBranch<R, SameEnvIndex> for SameEnv<Effect<B, E2, R>> {
  type Output = B;
  type Error = E2;

  fn into_branch(self) -> Effect<B, E2, R> {
    self.into_inner()
  }
}

/// A branch of `~(a, b, ..)` over the body's environment (`ctx` fixes `R`); the macro then
/// widens its error into the body's with `map_err` and [`Lift`].
pub fn branch<R, X, Is>(_ctx: &Ctx<R>, x: X) -> Effect<X::Output, X::Error, R>
where
  X: JoinBranch<R, Is>,
{
  x.into_branch()
}

/// A tuple of 2 to 8 branches over one environment and one error, joined by [`join_binds`].
#[diagnostic::on_unimplemented(
  message = "`~(..)` joins 2 to 8 effects over the same environment and error",
  label = "not a tuple of 2 to 8 effects"
)]
pub trait JoinTuple<R> {
  /// The tuple of the branches' values.
  type Output;
  /// The common (already widened) error.
  type Error;
  /// The branches as one effect: `all` over their erased values, re-tupled in input order.
  fn join(self) -> Effect<Self::Output, Self::Error, R>;
}

/// A branch's value, erased so that one homogeneous `fork_all` runs every branch.
type Slot = Box<dyn Any + Send>;

fn unslot<A: 'static>(slot: Option<Slot>) -> A {
  match slot.map(|s| s.downcast::<A>()) {
    Some(Ok(a)) => *a,
    _ => unreachable!("id_effect: `~(..)` join lost a branch value (kernel bug)"),
  }
}

macro_rules! join_tuple {
  ($($a:ident $i:tt),+) => {
    impl<E, R, $($a),+> JoinTuple<R> for ($(Effect<$a, E, R>,)+)
    where
      E: Send + 'static,
      R: Send + Sync + 'static,
      $($a: Send + 'static,)+
    {
      type Output = ($($a,)+);
      type Error = E;

      fn join(self) -> Effect<Self::Output, E, R> {
        let branches: Vec<Effect<Slot, E, R>> =
          vec![$(self.$i.map(|v| Box::new(v) as Slot)),+];
        all(branches, Concurrency::Unbounded).map(|slots| {
          let mut slots = slots.into_iter();
          ($(unslot::<$a>(slots.next()),)+)
        })
      }
    }
  };
}

join_tuple!(A0 0, A1 1);
join_tuple!(A0 0, A1 1, A2 2);
join_tuple!(A0 0, A1 1, A2 2, A3 3);
join_tuple!(A0 0, A1 1, A2 2, A3 3, A4 4);
join_tuple!(A0 0, A1 1, A2 2, A3 3, A4 4, A5 5);
join_tuple!(A0 0, A1 1, A2 2, A3 3, A4 4, A5 5, A6 6);
join_tuple!(A0 0, A1 1, A2 2, A3 3, A4 4, A5 5, A6 6, A7 7);

/// `~(a, b, ..)` inside `effect!`: runs the branches as child fibers (see the
/// [module docs](self)) and yields their values, or the first failure.
pub fn join_binds<R, T>(ctx: &Ctx<R>, branches: T) -> JoinFuture<T::Output, T::Error>
where
  T: JoinTuple<R>,
  T::Output: Send + 'static,
  T::Error: Send + 'static,
  R: Send + Sync + 'static,
{
  JoinFuture(ctx.bind_same_env(branches.join()))
}

/// The future of a `~(a, b, ..)` join: the bind of the joined effect, surfaced by its first
/// failure (the leftmost leaf of the cause) rather than its first typed failure.
#[must_use = "futures do nothing unless polled"]
#[derive(Debug)]
pub struct JoinFuture<B, E>(BindFuture<B, E>);

impl<B: 'static, E: 'static> Future for JoinFuture<B, E> {
  type Output = Result<B, E>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<B, E>> {
    self.get_mut().0.poll_exit(cx).map(surface_first)
  }
}

#[cfg(test)]
mod tests {
  use std::future::Future;
  use std::pin::Pin;
  use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
  use std::task::{Context, Poll, Wake, Waker};
  use std::time::{Duration, Instant};

  use super::*;
  use crate::capability::HNil;
  use crate::failure::{Cause, Exit};
  use crate::kernel::{LocalEffect, from_async, succeed, sync};
  use crate::runtime::{BoxFuture, Executor, FiberContext, LocalBoxFuture, NotLocal};

  struct NoExec;

  /// A waker that drops its wake-ups: these tests poll by hand.
  struct Ignore;

  #[allow(
    clippy::manual_noop_waker,
    reason = "the K-nb gate bans the std no-op waker anywhere under src/next"
  )]
  impl Wake for Ignore {
    fn wake(self: Arc<Self>) {}
  }

  impl Executor for NoExec {
    fn spawn(&self, _: BoxFuture<'static, ()>) {
      unreachable!("bind_local unit tests spawn nothing")
    }
    fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
      Err(NotLocal)
    }
    fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
      unreachable!("bind_local unit tests spawn nothing")
    }
    fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
      Box::pin(async {})
    }
    fn now(&self) -> Instant {
      Instant::now()
    }
  }

  /// Runs `~sub` (as `effect_local!` expands it) until `sub` is suspended, holding one
  /// mask level, then interrupts the fiber and polls again: `sub`'s finalizer runs (checked by
  /// the callers), the body does not continue, and no mask level leaks. Polled by hand, so the
  /// interrupt always lands on an already suspended runloop.
  fn interrupt_a_suspended_local_bind<X>(sub: X)
  where
    X: IntoLocal<u8, (), HNil> + 'static,
  {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let after = Arc::new(AtomicUsize::new(0));
    let after2 = Arc::clone(&after);
    let body: LocalEffect<u8, (), HNil> = LocalEffect::from_ctx(move |ctx| async move {
      let v = bind_local::<HNil, X, u8, (), HNil, HNil>(&ctx, sub).await?;
      after2.fetch_add(1, Ordering::SeqCst);
      Ok(v)
    });
    let mut run = body.run_on(Arc::clone(&fiber));
    let waker = Waker::from(Arc::new(Ignore));
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut run).poll(&mut cx).is_pending());
    assert!(fiber.mask() > 0, "the suspended bind holds a mask level");
    fiber.request_interrupt();
    let Poll::Ready(exit) = Pin::new(&mut run).poll(&mut cx) else {
      panic!("an interrupted bind completes")
    };
    assert!(
      matches!(exit, Exit::Failure(Cause::Interrupt(id)) if id == fiber.id()),
      "{exit:?}"
    );
    assert_eq!(after.load(Ordering::SeqCst), 0, "the body did not continue");
    assert_eq!(fiber.mask(), 0, "no mask level leaked");
  }

  #[test]
  fn an_interrupted_local_bind_of_a_local_effect_runs_its_finalizer() {
    let finalized = Arc::new(AtomicBool::new(false));
    let fin = Arc::clone(&finalized);
    interrupt_a_suspended_local_bind(
      LocalEffect::<u8, (), HNil>::from_future(|| async {
        std::future::pending::<()>().await;
        Ok(0)
      })
      .ensuring(LocalEffect::sync(move || fin.store(true, Ordering::SeqCst))),
    );
    assert!(finalized.load(Ordering::SeqCst), "sub-effect finalizer ran");
  }

  #[test]
  fn an_interrupted_local_bind_of_a_send_effect_runs_its_finalizer() {
    let finalized = Arc::new(AtomicBool::new(false));
    let fin = Arc::clone(&finalized);
    interrupt_a_suspended_local_bind(
      succeed::<u8, (), HNil>(0)
        .flat_map(|_| {
          from_async(|| async {
            std::future::pending::<()>().await;
            Ok(0)
          })
        })
        .ensuring(sync(move || fin.store(true, Ordering::SeqCst))),
    );
    assert!(finalized.load(Ordering::SeqCst), "sub-effect finalizer ran");
  }
}
