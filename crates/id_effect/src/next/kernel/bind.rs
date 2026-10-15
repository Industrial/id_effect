//! Typed capability access from a running effect: `Ctx::get`, `Ctx::bind` via
//! `Bindable<R, Is>`, and `BindFuture`. Filled by `k-typed-r`.
//!
//! Inside an async body (`Effect::from_ctx`, the `effect_next!` target) `ctx.get::<T>()` reads a
//! capability and `ctx.bind(sub).await` (what `~sub` expands to) runs a sub-effect whose
//! environment is a subset of the body's `R`, **on the same fiber**: the sub-effect sees the
//! fiber's interrupt flag and mask, and an interrupt reaches it while it is suspended, so its
//! finalizers run.
//!
//! `bind` takes any [`Bindable`] value, not just an `Effect`, so other binding forms (the
//! `SameEnv` wrapper of `k-macros-next`) are added as impls in their own files.
//!
//! # Outcomes
//!
//! The bind future yields `Result<B, E2>`: only the typed error reaches the body's `?`. A defect
//! or an interrupt of the sub-effect resumes as a panic that the enclosing runloop turns back
//! into the same cause (`Die` with its payload, or `Interrupt` of this fiber). A composite cause
//! yields its first typed failure (depth-first, as `catch` does); a composite with no typed
//! failure resumes as its first defect, else as an interrupt.

use std::future::Future;
use std::marker::PhantomData;
use std::panic::resume_unwind;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::anybox::{AnyEnv, unerase_exit};
use super::cause::{InterruptSignal, split_failure};
use super::ctx::Ctx;
use super::effect::{Effect, Node};
use super::runloop::{Runloop, settle_instr};
use crate::failure::Cause;
use crate::next::capability::{Embed, HList, Selector};
use crate::next::runtime::FiberContext;

impl<R> Ctx<R> {
  /// Capability `T` at the inferred index (what `~T` expands to). A missing `T` is a compile
  /// error, so this never fails.
  pub fn get<T, I>(&self) -> &Arc<T>
  where
    R: Selector<T, I>,
  {
    Selector::<T, I>::get(&*self.env)
  }

  /// Runs `sub` on this fiber, under the part of this body's environment it needs (what `~sub`
  /// expands to). See the [module docs](self) for how failures surface.
  ///
  /// Await binds one at a time: while a bind is suspended it keeps the fiber's interrupt for its
  /// own runloop (so the sub-effect's finalizers run), which two binds suspended at once under a
  /// `join` would hold off from each other.
  pub fn bind<X, Is>(&self, sub: X) -> BindFuture<X::Output, X::Error>
  where
    X: Bindable<R, Is>,
  {
    sub.bind_in(self)
  }
}

impl<R: Send + Sync + 'static> Ctx<R> {
  /// Binds an effect over exactly this body's environment: shares the `Arc` (no projection).
  /// The building block of the `SameEnv` impl of [`Bindable`] (`kernel/bind_same.rs`).
  pub(crate) fn bind_same_env<B, E2>(&self, eff: Effect<B, E2, R>) -> BindFuture<B, E2>
  where
    B: Send + 'static,
    E2: Send + 'static,
  {
    BindFuture::start(eff, &self.fiber, || Arc::clone(&self.env))
  }
}

/// A value that `Ctx<R>::bind` can run on the current fiber: an `Effect` whose environment `R`
/// embeds (at indices `Is`), or a wrapper that fixes its environment otherwise.
#[diagnostic::on_unimplemented(
  message = "`{Self}` cannot be bound with `~` in an effect over `{R}`",
  label = "not an effect over a part of `{R}`",
  note = "`~` / `Ctx::bind` takes an `Effect` whose capabilities are all in the enclosing effect's environment"
)]
pub trait Bindable<R, Is> {
  /// The sub-effect's success type.
  type Output;
  /// The sub-effect's typed error.
  type Error;
  /// Starts the bind on `ctx`'s fiber.
  fn bind_in(self, ctx: &Ctx<R>) -> BindFuture<Self::Output, Self::Error>;
}

impl<B, E2, Sub, R, Is> Bindable<R, Is> for Effect<B, E2, Sub>
where
  B: Send + 'static,
  E2: Send + 'static,
  Sub: HList,
  R: Embed<Sub, Is> + Send + Sync + 'static,
{
  type Output = B;
  type Error = E2;

  fn bind_in(self, ctx: &Ctx<R>) -> BindFuture<B, E2> {
    BindFuture::start(self, &ctx.fiber, || {
      // Same environment: share it. Otherwise project the subset (one `Arc` clone per cap).
      match (Arc::clone(&ctx.env) as AnyEnv).downcast::<Sub>() {
        Ok(same) => same,
        Err(_) => Arc::new(Embed::<Sub, Is>::project(&*ctx.env)),
      }
    })
  }
}

/// The future of a `~` bind ([`Ctx::bind`]): yields the sub-effect's value or typed error.
#[must_use = "futures do nothing unless polled"]
pub struct BindFuture<B, E> {
  state: BindState<B, E>,
}

enum BindState<B, E> {
  Ready(Option<Result<B, Cause<E>>>),
  Running {
    rl: Box<Runloop>,
    fiber: Arc<FiberContext>,
    /// `true` while this bind holds one mask level between polls (see `poll`).
    held: bool,
    _out: PhantomData<fn() -> (B, E)>,
  },
}

// Never pin-projected: the ready value is moved out by `Option::take`, the runloop is boxed.
impl<B, E> Unpin for BindFuture<B, E> {}

impl<B, E> std::fmt::Debug for BindFuture<B, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self.state {
      BindState::Ready(_) => "BindFuture::Ready",
      BindState::Running { .. } => "BindFuture::Running",
    })
  }
}

impl<B: Send + 'static, E: Send + 'static> BindFuture<B, E> {
  /// Runs `eff`'s synchronous prefix at once (no runloop for ready or fused effects) and puts the
  /// rest on a nested runloop over `env()` and `fiber`.
  fn start<Sub: Send + Sync + 'static>(
    eff: Effect<B, E, Sub>,
    fiber: &Arc<FiberContext>,
    env: impl FnOnce() -> Arc<Sub>,
  ) -> Self {
    let mut node = eff.node;
    // A ready effect needs no environment at all; anything else builds it once.
    let sub: Arc<Sub> = match node {
      Node::Pure(r) => {
        return Self {
          state: BindState::Ready(Some(r)),
        };
      }
      _ => env(),
    };
    let instr = loop {
      match node {
        Node::Pure(r) => {
          return Self {
            state: BindState::Ready(Some(r)),
          };
        }
        Node::Sync(f, _) => node = f(&sub).node,
        Node::Op(i) => break i,
      }
    };
    let sub = sub as AnyEnv;
    // The fiber-free prefix (`map`/`flat_map` over ready values) runs here, without a runloop.
    match settle_instr(instr, &sub) {
      Ok(exit) => Self {
        state: BindState::Ready(Some(unerase_exit::<B, E>(exit))),
      },
      Err(rest) => Self {
        state: BindState::Running {
          rl: Box::new(Runloop::new(rest, sub, Arc::clone(fiber))),
          fiber: Arc::clone(fiber),
          held: false,
          _out: PhantomData,
        },
      },
    }
  }
}

/// Hands a sub-effect's outcome to the body: the typed error as `Err`, every other cause as a
/// panic that the enclosing runloop converts back (`cause_from_panic`).
pub(crate) fn surface<B, E>(r: Result<B, Cause<E>>) -> Result<B, E> {
  let cause = match r {
    Ok(b) => return Ok(b),
    Err(c) => c,
  };
  let cause = match split_failure::<E, E>(cause) {
    Ok(e) => return Err(e),
    Err(c) => c,
  };
  resume_unwind(non_fail_payload(cause))
}

/// The panic payload that re-raises a cause with no typed failure in the enclosing runloop.
fn non_fail_payload<E>(cause: Cause<E>) -> Box<dyn std::any::Any + Send> {
  fn first_die<E>(c: &Cause<E>) -> Option<&crate::failure::Defect> {
    match c {
      Cause::Die(d) => Some(d),
      Cause::Both(l, r) | Cause::Then(l, r) => first_die(l).or_else(|| first_die(r)),
      _ => None,
    }
  }
  fn first_interrupt<E>(c: &Cause<E>) -> Option<crate::runtime::FiberId> {
    match c {
      Cause::Interrupt(id) => Some(*id),
      Cause::Both(l, r) | Cause::Then(l, r) => first_interrupt(l).or_else(|| first_interrupt(r)),
      _ => None,
    }
  }
  if let Some(d) = first_die(&cause) {
    // The root `Defect` keeps a panic payload in a take-once slot; a defect built from a message
    // (or whose payload was already taken) re-raises its message, which `Defect::from_panic`
    // turns back into the same message.
    return d
      .take_payload()
      .unwrap_or_else(|| Box::new(d.message().to_owned()));
  }
  match first_interrupt(&cause) {
    Some(id) => Box::new(InterruptSignal(id)),
    None => Box::new(String::from("next: empty cause in a `~` bind (kernel bug)")),
  }
}

impl<B: 'static, E: 'static> Future for BindFuture<B, E> {
  type Output = Result<B, E>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<B, E>> {
    match &mut self.get_mut().state {
      BindState::Ready(r) => match r.take() {
        Some(r) => Poll::Ready(surface(r)),
        None => unreachable!("next: bind polled after completion"),
      },
      BindState::Running {
        rl, fiber, held, ..
      } => {
        // While suspended, this bind holds one mask level so that the enclosing runloop does
        // not drop the body's future (and this nested runloop with it) on an interrupt. It is
        // released before every poll: the nested runloop then sees the interrupt itself,
        // unwinds its frames (running finalizers) and reports `Interrupt`.
        if std::mem::take(held) {
          fiber.set_mask(fiber.mask().saturating_sub(1));
        }
        match Pin::new(&mut **rl).poll(cx) {
          Poll::Ready(exit) => Poll::Ready(surface(unerase_exit::<B, E>(exit))),
          Poll::Pending => {
            fiber.set_mask(fiber.mask() + 1);
            *held = true;
            Poll::Pending
          }
        }
      }
    }
  }
}

impl<B, E> Drop for BindFuture<B, E> {
  fn drop(&mut self) {
    // A bind dropped while suspended (the body was abandoned) gives its mask level back.
    if let BindState::Running {
      fiber, held: true, ..
    } = &self.state
    {
      fiber.set_mask(fiber.mask().saturating_sub(1));
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
  use std::sync::mpsc;
  use std::time::{Duration, Instant};

  use super::*;
  use crate::failure::Exit;
  use crate::next::capability::{HCons, HNil};
  use crate::next::kernel::{Instr, MaskOp, erase};
  use crate::next::runtime::{BoxFuture, Executor, LocalBoxFuture, NotLocal};

  struct NoExec;

  impl Executor for NoExec {
    fn spawn(&self, _: BoxFuture<'static, ()>) {
      unreachable!("bind unit tests spawn nothing")
    }
    fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
      Err(NotLocal)
    }
    fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
      unreachable!("bind unit tests spawn nothing")
    }
    fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
      Box::pin(async {})
    }
    fn now(&self) -> Instant {
      Instant::now()
    }
  }

  struct Cap;

  /// `body` with a finalizer that runs however it ends (the shape of `Effect::ensuring`, built
  /// from raw instructions so this test does not depend on `k-fiber-tree`).
  fn with_finalizer<B: Send + 'static, R: Send + Sync + 'static>(
    body: Effect<B, (), R>,
    ran: Arc<AtomicBool>,
  ) -> Effect<B, (), R> {
    Effect::from_instr(Instr::Mask(
      Box::new(Instr::Fold(
        Box::new(Instr::Mask(Box::new(body.into_instr()), MaskOp::Set(0))),
        Box::new(move |exit| {
          ran.store(true, Ordering::SeqCst);
          Instr::Pure(exit)
        }),
      )),
      MaskOp::Inc,
    ))
  }

  /// A sub-effect that signals `started` and then never completes.
  fn suspended(started: mpsc::Sender<()>) -> Effect<u8, (), HCons<Cap, HNil>> {
    Effect::from_instr(Instr::Future(Box::pin(async move {
      let _ = started.send(());
      std::future::pending::<()>().await;
      Ok(erase(0_u8))
    })))
  }

  #[test]
  fn interrupting_a_suspended_bind_runs_the_sub_effects_finalizer() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let finalized = Arc::new(AtomicBool::new(false));
    let after = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    let sub = with_finalizer(suspended(tx), Arc::clone(&finalized));
    let after2 = Arc::clone(&after);
    let outer: Effect<u8, (), HCons<Cap, HCons<u16, HNil>>> =
      Effect::from_ctx(move |ctx| async move {
        let v = ctx.bind(sub).await?;
        after2.fetch_add(1, Ordering::SeqCst);
        Ok(v)
      });
    let runnable = outer.provide_all(HCons::new(Cap, HCons::new(7_u16, HNil)));
    let f2 = Arc::clone(&fiber);
    let driver = std::thread::spawn(move || pollster::block_on(runnable.run_on(f2)));
    rx.recv().expect("the sub-effect started");
    fiber.request_interrupt();
    let exit = driver.join().expect("driver thread");
    assert!(
      matches!(exit, Exit::Failure(Cause::Interrupt(id)) if id == fiber.id()),
      "{exit:?}"
    );
    assert!(finalized.load(Ordering::SeqCst), "sub-effect finalizer ran");
    assert_eq!(after.load(Ordering::SeqCst), 0, "the body did not continue");
    assert_eq!(fiber.mask(), 0, "no mask level leaked");
  }

  #[test]
  fn a_bind_dropped_while_suspended_releases_its_mask_level() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let (tx, _rx) = mpsc::channel();
    let outer: Effect<u32, (), HCons<Cap, HNil>> = Effect::from_ctx(move |ctx| async move {
      let mut b = ctx.bind(suspended(tx));
      let pending =
        std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut b).poll(cx).is_pending())).await;
      let held = ctx.fiber().mask();
      drop(b);
      Ok(u32::from(pending) * 100 + held * 10 + ctx.fiber().mask())
    });
    let exit = pollster::block_on(outer.provide(Cap).run_on(Arc::clone(&fiber)));
    assert_eq!(exit, Exit::Success(110));
  }

  #[test]
  fn defects_surface_with_their_payload_and_messages_survive() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let outer: Effect<u8, (), HCons<Cap, HNil>> = Effect::from_ctx(|ctx| async move {
      let sub: Effect<u8, (), HCons<Cap, HNil>> =
        Effect::from_instr(Instr::Future(Box::pin(async { panic!("deep") })));
      ctx.bind(sub).await
    });
    match pollster::block_on(outer.provide(Cap).run_on(Arc::clone(&fiber))) {
      Exit::Failure(Cause::Die(d)) => {
        assert_eq!(d.message(), "deep");
        assert_eq!(d.with_payload(|s: &&str| *s), Some("deep"));
      }
      other => panic!("expected Die, got {other:?}"),
    }
    let p = non_fail_payload::<()>(Cause::both(
      Cause::interrupt(fiber.id()),
      Cause::die("message only"),
    ));
    assert_eq!(
      p.downcast_ref::<String>().map(String::as_str),
      Some("message only")
    );
    let p = non_fail_payload::<()>(Cause::then(
      Cause::interrupt(fiber.id()),
      Cause::interrupt(crate::runtime::FiberId(0)),
    ));
    assert!(matches!(p.downcast_ref::<InterruptSignal>(), Some(s) if s.0 == fiber.id()));
  }
}
