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
  /// Several binds may be suspended at once (under a `join`): each still sees an interrupt of
  /// the fiber when it is next polled (`BindHold` in `kernel/bind.rs`). They share the fiber,
  /// so they run interleaved, not in parallel; `~(a, b, ..)` in `effect_next!` forks them.
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
    /// This bind's share of the fiber mask between polls.
    hold: BindHold,
    _out: PhantomData<fn() -> (B, E)>,
  },
}

/// One suspended bind's share of the fiber mask.
const HOLD: u32 = 1 << 16;
/// The mask bits below [`HOLD`]: mask regions (`MaskOp`), which nest far less than 2^16 deep.
const REGIONS: u32 = HOLD - 1;

/// How a nested runloop on the enclosing runloop's fiber (a `~` bind) holds the fiber mask while
/// it is suspended.
///
/// While suspended, a bind adds [`HOLD`] to the fiber mask so that the enclosing runloop does not
/// drop the body's future (and the nested runloop with it) on an interrupt: the nested runloop
/// must see the interrupt itself, unwind its frames (running finalizers) and report `Interrupt`.
///
/// Holds count in units of `HOLD`, above the mask-region bits, so a bind can tell them apart:
/// before it polls its runloop it removes its own hold **and** the holds of every bind that is
/// not nested inside it (siblings under a `join`, and the binds that enclose it), and puts them
/// back after the poll. Without that, two binds suspended at once would each see the other's
/// level and hold an interrupt off each other forever (execution-log follow-up (d)). The holds
/// of binds nested inside this one (`inner`) stay: they belong to its own runloop.
///
/// A mask region a suspended nested runloop is inside still applies to the whole fiber, siblings
/// included (they share the fiber's mask).
#[derive(Debug, Default)]
pub(crate) struct BindHold {
  /// `true` while this bind holds `HOLD` between polls.
  held: bool,
  /// The holds (`HOLD` multiples) its own runloop left at the end of its last poll.
  inner: u32,
}

impl BindHold {
  /// Before a poll: lifts this bind's hold and every hold not nested inside it. Returns what was
  /// lifted besides its own hold (for [`settle`](Self::settle)).
  #[inline]
  pub(crate) fn lift(&mut self, fiber: &FiberContext) -> u32 {
    let cur = fiber.mask();
    let m = if std::mem::take(&mut self.held) {
      cur.saturating_sub(HOLD)
    } else {
      cur
    };
    let outer = (m & !REGIONS).saturating_sub(self.inner);
    if m - outer != cur {
      fiber.set_mask(m - outer);
    }
    outer
  }

  /// After a poll: gives back the `outer` holds and, while still `pending`, takes this bind's own.
  #[inline]
  pub(crate) fn settle(&mut self, fiber: &FiberContext, outer: u32, pending: bool) {
    let after = fiber.mask();
    if pending {
      self.inner = after & !REGIONS;
      self.held = true;
      fiber.set_mask(after + outer + HOLD);
    } else {
      self.inner = 0;
      if outer != 0 {
        fiber.set_mask(after + outer);
      }
    }
  }

  /// A bind dropped while suspended (its body was abandoned) gives its hold back. Binds nested
  /// inside it give theirs back as its runloop is dropped.
  #[inline]
  pub(crate) fn release(&mut self, fiber: &FiberContext) {
    if std::mem::take(&mut self.held) {
      fiber.set_mask(fiber.mask().saturating_sub(HOLD));
    }
  }
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
          hold: BindHold::default(),
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

/// Hands a joined sub-effect's outcome to the body by its **first** failure: the leftmost leaf of
/// the cause, which `fork_all` puts first (`zip_par`'s shape). A typed failure is `Err`; a first
/// defect or interrupt resumes as a panic, even when a sibling also failed with a typed error.
/// Sibling failures combined in with `Both` are dropped: a `Result` holds one error.
pub(crate) fn surface_first<B, E>(r: Result<B, Cause<E>>) -> Result<B, E> {
  let mut cause = match r {
    Ok(b) => return Ok(b),
    Err(c) => c,
  };
  loop {
    cause = match cause {
      Cause::Fail(e) => return Err(e),
      Cause::Both(l, _) | Cause::Then(l, _) => *l,
      leaf => resume_unwind(non_fail_payload(leaf)),
    };
  }
}

impl<B: 'static, E: 'static> BindFuture<B, E> {
  /// Polls the sub-effect to its exit (the whole cause, not yet surfaced).
  #[inline]
  pub(crate) fn poll_exit(&mut self, cx: &mut Context<'_>) -> Poll<Result<B, Cause<E>>> {
    match &mut self.state {
      BindState::Ready(r) => match r.take() {
        Some(r) => Poll::Ready(r),
        None => unreachable!("next: bind polled after completion"),
      },
      BindState::Running {
        rl, fiber, hold, ..
      } => {
        // See `BindHold`: the nested runloop is polled without this bind's hold or any hold
        // outside it, so it sees an interrupt itself and unwinds (running finalizers).
        let outer = hold.lift(fiber);
        let polled = Pin::new(&mut **rl).poll(cx);
        hold.settle(fiber, outer, polled.is_pending());
        polled.map(unerase_exit::<B, E>)
      }
    }
  }
}

impl<B: 'static, E: 'static> Future for BindFuture<B, E> {
  type Output = Result<B, E>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<B, E>> {
    self.get_mut().poll_exit(cx).map(surface)
  }
}

impl<B, E> Drop for BindFuture<B, E> {
  fn drop(&mut self) {
    if let BindState::Running { fiber, hold, .. } = &mut self.state {
      hold.release(fiber);
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
      Ok(u32::from(pending) * 100 + u32::from(held == HOLD) * 10 + ctx.fiber().mask())
    });
    let exit = pollster::block_on(outer.provide(Cap).run_on(Arc::clone(&fiber)));
    assert_eq!(exit, Exit::Success(110));
  }

  /// Execution-log follow-up (d): two binds suspended at once under a `join` each held one
  /// mask level, so neither nested runloop ever saw an interrupt and the fiber hung.
  #[test]
  fn two_binds_suspended_under_a_join_do_not_hold_an_interrupt_off_each_other() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let fin_a = Arc::new(AtomicBool::new(false));
    let fin_b = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let a = with_finalizer(suspended(tx.clone()), Arc::clone(&fin_a));
    let b = with_finalizer(suspended(tx), Arc::clone(&fin_b));
    let outer: Effect<(u8, u8), (), HCons<Cap, HNil>> = Effect::from_ctx(move |ctx| async move {
      let (x, y) = futures::future::join(ctx.bind(a), ctx.bind(b)).await;
      Ok((x?, y?))
    });
    let runnable = outer.provide(Cap);
    let f2 = Arc::clone(&fiber);
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
      let _ = done_tx.send(pollster::block_on(runnable.run_on(f2)));
    });
    rx.recv().expect("a started");
    rx.recv().expect("b started");
    fiber.request_interrupt();
    let exit = done_rx
      .recv_timeout(Duration::from_secs(10))
      .expect("the interrupt reaches the suspended binds (no hang)");
    assert!(
      matches!(exit, Exit::Failure(Cause::Interrupt(id)) if id == fiber.id()),
      "{exit:?}"
    );
    // The first bind polled sees the interrupt and unwinds; its `Interrupt` then unwinds the
    // body, which drops the sibling (a same-fiber `join` cannot wait for it; `~(a, b)` forks).
    assert!(fin_a.load(Ordering::SeqCst), "a's finalizer ran");
    assert_eq!(fiber.mask(), 0, "no hold leaked");
  }

  /// Hand-polled: a bind lifts its sibling's hold only while it polls its own runloop, and keeps
  /// the holds of binds nested inside it.
  #[test]
  fn a_bind_polls_without_its_siblings_holds_and_with_its_nested_ones() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let mut a = BindHold::default();
    let mut b = BindHold::default();
    // a and b suspended side by side, a mask region (1) around both.
    fiber.set_mask(1);
    let out = a.lift(&fiber);
    assert_eq!((out, fiber.mask()), (0, 1));
    a.settle(&fiber, out, true);
    let out = b.lift(&fiber);
    assert_eq!((out, fiber.mask()), (HOLD, 1), "b polls without a's hold");
    b.settle(&fiber, out, true);
    assert_eq!(fiber.mask(), 1 + 2 * HOLD);
    // a polls again; a bind nested inside a (c) suspends during that poll.
    let out = a.lift(&fiber);
    assert_eq!((out, fiber.mask()), (HOLD, 1));
    let mut c = BindHold::default();
    let out_c = c.lift(&fiber);
    c.settle(&fiber, out_c, true);
    a.settle(&fiber, out, true);
    assert_eq!(fiber.mask(), 1 + 3 * HOLD);
    // The next poll of a keeps c's hold (it is a's own); c then sees only its own.
    let out = a.lift(&fiber);
    assert_eq!((out, fiber.mask()), (HOLD, 1 + HOLD));
    let out_c = c.lift(&fiber);
    assert_eq!((out_c, fiber.mask()), (0, 1));
    c.settle(&fiber, out_c, false);
    a.settle(&fiber, out, false);
    assert_eq!(fiber.mask(), 1 + HOLD, "only b still holds");
    b.release(&fiber);
    assert_eq!(fiber.mask(), 1);
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
