//! Support items for the `*_next!` macro expansions, reached as `next::__private`. Not API.
//!
//! - [`Lift`] with [`LiftNever`] / [`LiftFrom`]: error widening for `~` binds by autoref
//!   specialisation (spike A `lib.rs:46-84`). `(&&Lift::new(e)).lift()` picks `LiftNever` for a
//!   `Never` error (widens to anything) and `LiftFrom` otherwise (`E: From<E2>`).
//! - [`bind_local`]: what `~expr` expands to inside `effect_local_next!`.
//! - [`CapsChecked`]: the identity projection `caps_next!` wraps a de-duplicated list in when it
//!   also reports a duplicate (so the error does not cascade).

use std::cell::Cell;
use std::future::Future;
use std::sync::Arc;

use super::anybox::AnyEnv;
use super::bind::surface;
use super::cause::from_exit;
use super::ctx::Ctx;
use super::local::{HoldMask, IntoLocal};
use crate::next::capability::Embed;
use crate::runtime::Never;

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
      None => unreachable!("next: `~` error lifted twice (macro bug)"),
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
      None => unreachable!("next: `~` error lifted twice (macro bug)"),
    }
  }
}

/// `~sub` inside `effect_local_next!`: runs a [`LocalEffect`](super::LocalEffect) or a `Send`
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

/// The identity on `Self`, with a const argument that carries `caps_next!`'s duplicate errors.
pub trait CapsChecked<const N: usize> {
  /// `Self`.
  type Out;
}

impl<T, const N: usize> CapsChecked<N> for T {
  type Out = T;
}

#[cfg(test)]
mod tests {
  use std::future::Future;
  use std::pin::Pin;
  use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
  use std::task::{Context, Poll, Wake, Waker};
  use std::time::{Duration, Instant};

  use super::*;
  use crate::failure::{Cause, Exit};
  use crate::next::capability::HNil;
  use crate::next::kernel::{LocalEffect, from_async, succeed, sync};
  use crate::next::runtime::{BoxFuture, Executor, FiberContext, LocalBoxFuture, NotLocal};

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

  /// Runs `~sub` (as `effect_local_next!` expands it) until `sub` is suspended, holding one
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
