//! `LocalEffect` (the opt-in `!Send` effect), `IntoLocal` and `.local()`. Filled by `k-edge`.
//!
//! ADR 0011 D4: `LocalEffect<A, E, R>` is the `!Send` twin of [`Effect`] for `Rc`/`RefCell`
//! code. Unlike the spike's thin async wrapper it has its own erased runloop (same shape as
//! `kernel/runloop.rs`, with `!Send` boxes), so it gets the same semantics:
//!
//! - an inline `Ready` node (no allocation for ready values); anything else is an erased program;
//! - the combinators `map`/`flat_map`/`then`/`zip`/`map_err`/`catch`/`exit`/`widen`, the
//!   resource combinators `ensuring`/`uninterruptible`, and `provide`/`provide_env`;
//! - interrupts (checked at every instruction boundary and before every poll, honoured outside
//!   masks), panic capture per step (`Die`), a cooperative yield budget, and an explicit frame
//!   stack (deep `flat_map` chains do not grow the native stack).
//!
//! `Effect → LocalEffect` is free ([`Effect::local`], [`IntoLocal`]): the `Send` program runs as
//! a nested runloop on the same fiber and env, so interrupts and masks carry across.
//!
//! **Limits.** A `LocalEffect` cannot `fork` (a forked fiber may move threads). It runs on the
//! thread that polls it: [`LocalEffect::block_on`] (the calling thread, parked on a real waker),
//! [`LocalEffect::run_on`] (any `!Send`-capable driver), or [`LocalEffect::run_on_local`], which
//! starts it on an executor's `spawn_local` worker and returns a `Send` [`Effect`] awaiting it.
//!
//! **Executor story.** `TokioExecutor::spawn_local` is always `Err(NotLocal)` (no `LocalSet`
//! integration yet), so on tokio use `block_on` or `run_on` under a `LocalSet`. A `TestExecutor`
//! supports `spawn_local` on the thread that created it, so `run_on_local` works there.
//!
//! The twin runloop is hand-written here, not generated from one template with the `Send` one
//! (design §1.2); `k-runloop-opt` changes to `runloop.rs` have to be mirrored (the shapes match
//! one to one: `Instr`/`LInstr`, `Frame`/`LFrame`, `Runloop`/`LocalRunloop`).

use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use super::anybox::{AnyEnv, downcast_env, downcast_env_arc, erase_exit, unerase_exit};
use super::bind::BindHold;
use super::cause::{
  cause_from_panic, from_exit, map_fail_mut, map_fail_once, split_failure, to_exit,
};
use super::ctx::Ctx;
use super::effect::{Effect, Node};
use super::runloop::{Instr, Runloop};
use crate::failure::{Cause, Defect, Exit};
use crate::next::capability::{HNil, Plucker};
use crate::next::runtime::{
  BlockOnInAsync, Executor, FiberContext, LocalBoxFuture, NotLocal, Runtime,
};
use crate::next::runtime::{finish_children, in_async_context, park_on};
use crate::runtime::Never;

/// An erased `!Send` runloop value.
type LBox = Box<dyn Any>;
/// The erased outcome of one local runloop step.
type LExit = Result<LBox, Cause<LBox>>;
type LCont = Box<dyn FnOnce(LBox) -> LInstr>;
type LHandler = Box<dyn FnOnce(LExit) -> LInstr>;
type LStepFn = Box<dyn FnOnce(&AnyEnv, &Arc<FiberContext>) -> LInstr>;
type LAsyncFn = Box<dyn FnOnce(&AnyEnv, &Arc<FiberContext>) -> LocalBoxFuture<'static, LExit>>;
type LProvideFn = Box<dyn FnOnce(&AnyEnv) -> AnyEnv>;

fn lerase<T: 'static>(t: T) -> LBox {
  Box::new(t)
}

fn lunbox<T: 'static>(b: LBox) -> T {
  match b.downcast::<T>() {
    Ok(t) => *t,
    Err(_) => panic!("next: local erased value type mismatch (kernel bug)"),
  }
}

fn lerase_exit<A: 'static, E: 'static>(r: Result<A, Cause<E>>) -> LExit {
  match r {
    Ok(a) => Ok(lerase(a)),
    Err(c) => Err(map_fail_mut(c, &mut lerase)),
  }
}

fn lunerase_exit<A: 'static, E: 'static>(r: LExit) -> Result<A, Cause<E>> {
  match r {
    Ok(a) => Ok(lunbox(a)),
    Err(c) => Err(map_fail_mut(c, &mut lunbox)),
  }
}

/// Instructions of the local runloop (the `!Send` mirror of `Instr`).
enum LInstr {
  Pure(LExit),
  /// Compute the next instruction from the env and the fiber (sync steps, binds of ready values).
  Step(LStepFn),
  /// Suspend on a `'static`, `!Send` future built from the env and the fiber.
  Async(LAsyncFn),
  /// `inner` followed by `conts` in order (repeated `map`/`flat_map` push here). Boxed as one
  /// unit, as `Instr::Seq` is: moving the effect while building stays a tag and a pointer.
  Seq(Box<(LInstr, Vec<LCont>)>),
  /// Runs on success and failure (`catch`, `exit`, `map_err`, finalizers).
  Fold(Box<LInstr>, LHandler),
  /// Run the inner instruction under a different env (`provide`).
  Provide(Box<LInstr>, LProvideFn),
  /// Run the inner instruction under a different mask: `None` = one level deeper
  /// (uninterruptible), `Some(m)` = restore an earlier mask `m`.
  Mask(Box<LInstr>, Option<u32>),
}

enum LFrame {
  Cont(LCont),
  Fold(LHandler),
  Env(AnyEnv),
  Mask(u32),
}

/// Instructions per poll before the local runloop yields (same budget as the `Send` runloop).
const BUDGET: u32 = 2048;

/// The `!Send` erased interpreter (mirrors `Runloop`).
struct LocalRunloop {
  cur: Option<LInstr>,
  fut: Option<LocalBoxFuture<'static, LExit>>,
  stack: Vec<LFrame>,
  env: AnyEnv,
  fiber: Arc<FiberContext>,
}

impl LocalRunloop {
  /// Pops frames until a continuation consumes `exit` and returns its next instruction, or
  /// `LInstr::Pure(exit)` once the stack is empty (mirrors `Runloop::unwind`).
  #[inline(always)]
  fn unwind(&mut self, mut exit: LExit) -> LInstr {
    while let Some(frame) = self.stack.pop() {
      match frame {
        LFrame::Cont(k) => match exit {
          Ok(v) => return k(v),
          Err(c) => exit = Err(c),
        },
        LFrame::Fold(h) => return h(exit),
        LFrame::Env(e) => self.env = e,
        LFrame::Mask(m) => {
          self.fiber.set_mask(m);
          if exit.is_ok() && self.fiber.should_interrupt() {
            exit = Err(Cause::Interrupt(self.fiber.id()));
          }
        }
      }
    }
    LInstr::Pure(exit)
  }
}

impl Future for LocalRunloop {
  type Output = LExit;

  /// One poll under a single panic boundary (mirrors `Runloop::poll`): a panicking step
  /// re-enters the interpreter as `Die` and unwinds the frame stack.
  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<LExit> {
    let this = &mut *self;
    let mut budget = BUDGET;
    loop {
      match catch_unwind(AssertUnwindSafe(|| this.drive(cx, &mut budget))) {
        Ok(p) => return p,
        Err(payload) => {
          this.fut = None;
          this.cur = Some(LInstr::Pure(Err(cause_from_panic(payload))));
        }
      }
    }
  }
}

impl LocalRunloop {
  /// The interpreter loop (mirrors `Runloop::drive`: the instruction lives in a local, fresh
  /// futures are polled at once, binds go through [`feed`](Self::feed)).
  fn drive(&mut self, cx: &mut Context<'_>, budget: &mut u32) -> Poll<LExit> {
    let mut instr = match self.fut.as_mut() {
      Some(fut) => {
        let exit = if self.fiber.should_interrupt() {
          Err(Cause::Interrupt(self.fiber.id()))
        } else {
          match fut.as_mut().poll(cx) {
            Poll::Ready(exit) => exit,
            Poll::Pending => match self.suspend(cx) {
              Some(i) => {
                self.cur = Some(i);
                return self.drive(cx, budget);
              }
              None => return Poll::Pending,
            },
          }
        };
        self.fut = None;
        LInstr::Pure(exit)
      }
      None => match self.cur.take() {
        Some(i) => i,
        None => unreachable!("next: local runloop polled after completion"),
      },
    };
    loop {
      if !matches!(instr, LInstr::Pure(Err(_))) && self.fiber.should_interrupt() {
        instr = LInstr::Pure(Err(Cause::Interrupt(self.fiber.id())));
      }
      instr = match instr {
        LInstr::Pure(exit) => {
          if self.stack.is_empty() {
            return Poll::Ready(exit);
          }
          match self.feed(exit, cx, budget) {
            Some(next) => next,
            None => return Poll::Pending,
          }
        }
        LInstr::Step(f) => f(&self.env, &self.fiber),
        LInstr::Async(f) => {
          let fut = f(&self.env, &self.fiber);
          match self.start(fut, cx) {
            Some(next) => next,
            None => return Poll::Pending,
          }
        }
        LInstr::Seq(body) => {
          let (inner, conts) = *body;
          self.stack.extend(conts.into_iter().rev().map(LFrame::Cont));
          inner
        }
        LInstr::Fold(inner, h) => {
          self.stack.push(LFrame::Fold(h));
          *inner
        }
        LInstr::Provide(inner, f) => {
          let new_env = f(&self.env);
          let old = std::mem::replace(&mut self.env, new_env);
          self.stack.push(LFrame::Env(old));
          *inner
        }
        LInstr::Mask(inner, set) => {
          let old = self.fiber.mask();
          self.stack.push(LFrame::Mask(old));
          self.fiber.set_mask(set.unwrap_or(old + 1));
          *inner
        }
      };
      *budget -= 1;
      if *budget == 0 {
        self.cur = Some(instr);
        cx.waker().wake_by_ref();
        return Poll::Pending;
      }
    }
  }

  /// The bind fast path (mirrors `Runloop::feed`): a success into the `Cont` frame on top, then
  /// on while continuations return ready values or async steps that are ready at once.
  #[inline(always)]
  fn feed(&mut self, mut exit: LExit, cx: &mut Context<'_>, budget: &mut u32) -> Option<LInstr> {
    loop {
      let v = match (exit, self.stack.last()) {
        (Ok(v), Some(LFrame::Cont(_))) => v,
        (exit, _) => return Some(self.unwind(exit)),
      };
      let Some(LFrame::Cont(k)) = self.stack.pop() else {
        unreachable!("next: checked Cont frame (kernel bug)")
      };
      exit = match k(v) {
        LInstr::Pure(e) => e,
        LInstr::Async(f) => {
          if self.fiber.should_interrupt() {
            Err(Cause::Interrupt(self.fiber.id()))
          } else {
            let mut fut = f(&self.env, &self.fiber);
            match fut.as_mut().poll(cx) {
              Poll::Ready(e) => e,
              Poll::Pending => {
                self.fut = Some(fut);
                return self.suspend(cx);
              }
            }
          }
        }
        other => return Some(other),
      };
      if exit.is_ok() && self.fiber.should_interrupt() {
        exit = Err(Cause::Interrupt(self.fiber.id()));
      }
      *budget -= 1;
      if *budget == 0 {
        self.cur = Some(LInstr::Pure(exit));
        cx.waker().wake_by_ref();
        return None;
      }
    }
  }

  /// Polls a fresh future once: its exit as the next instruction, or `None` once suspended.
  #[inline]
  fn start(
    &mut self,
    mut fut: LocalBoxFuture<'static, LExit>,
    cx: &mut Context<'_>,
  ) -> Option<LInstr> {
    match fut.as_mut().poll(cx) {
      Poll::Ready(exit) => Some(LInstr::Pure(exit)),
      Poll::Pending => {
        self.fut = Some(fut);
        self.suspend(cx)
      }
    }
  }

  /// Registers the waker and re-checks the interrupt flag (mirrors `Runloop::suspend`).
  fn suspend(&mut self, cx: &mut Context<'_>) -> Option<LInstr> {
    self.fiber.register_waker(cx.waker());
    if self.fiber.should_interrupt() {
      self.fut = None;
      Some(LInstr::Pure(Err(Cause::Interrupt(self.fiber.id()))))
    } else {
      None
    }
  }
}

/// The opt-in `!Send` effect (ADR 0011 D4): yields `A`, fails with `E`, needs `R`.
///
/// It is never `Send`, so it may capture and hold `Rc`/`RefCell` values across awaits. See the
/// module docs for its semantics, its limits (no `fork`) and how to run it.
#[must_use = "effects are lazy; run them with `block_on`, `run_on` or `run_on_local`"]
pub struct LocalEffect<A, E = Never, R = HNil> {
  node: LNode<A, E, R>,
}

enum LNode<A, E, R> {
  /// Ready outcome (inline, no allocation).
  Ready(Result<A, Cause<E>>),
  /// Erased local program; `R` is tracked in the type only.
  Op(LInstr, PhantomData<fn() -> R>),
}

impl<A, E, R> std::fmt::Debug for LocalEffect<A, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self.node {
      LNode::Ready(_) => "LocalEffect::Ready",
      LNode::Op(..) => "LocalEffect::Op",
    })
  }
}

impl<A, E, R> LocalEffect<A, E, R> {
  const fn ready(r: Result<A, Cause<E>>) -> Self {
    Self {
      node: LNode::Ready(r),
    }
  }

  fn from_instr(i: LInstr) -> Self {
    Self {
      node: LNode::Op(i, PhantomData),
    }
  }

  /// A ready success (no allocation).
  pub const fn succeed(a: A) -> Self {
    Self::ready(Ok(a))
  }

  /// A ready typed failure (no allocation).
  pub const fn fail(e: E) -> Self {
    Self::ready(Err(Cause::Fail(e)))
  }

  /// A defect: the effect dies with `defect`.
  pub fn die(defect: impl Into<Defect>) -> Self {
    Self::ready(Err(Cause::Die(defect.into())))
  }
}

impl<A, E, R> LocalEffect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: Send + Sync + 'static,
{
  fn into_instr(self) -> LInstr {
    match self.node {
      LNode::Ready(r) => LInstr::Pure(lerase_exit(r)),
      LNode::Op(i, _) => i,
    }
  }

  fn step(f: impl FnOnce(&AnyEnv, &Arc<FiberContext>) -> LInstr + 'static) -> Self {
    Self::from_instr(LInstr::Step(Box::new(f)))
  }

  /// A lazy synchronous computation. A panic in `f` becomes `Cause::Die`.
  pub fn sync(f: impl FnOnce() -> A + 'static) -> Self {
    Self::step(move |_, _| LInstr::Pure(Ok(lerase(f()))))
  }

  /// Defers building an effect until it runs.
  pub fn suspend(f: impl FnOnce() -> Self + 'static) -> Self {
    Self::step(move |_, _| f().into_instr())
  }

  /// A real `!Send` async step; `f` runs when the step starts.
  pub fn from_future<F, Fut>(f: F) -> Self
  where
    F: FnOnce() -> Fut + 'static,
    Fut: Future<Output = Result<A, E>> + 'static,
  {
    Self::from_instr(LInstr::Async(Box::new(move |_, _| {
      let fut = f();
      Box::pin(async move { lerase_exit(fut.await.map_err(Cause::Fail)) })
    })))
  }

  /// An async body that owns a [`Ctx<R>`] (the `effect_local_next!` target).
  pub fn from_ctx<F, Fut>(f: F) -> Self
  where
    F: FnOnce(Ctx<R>) -> Fut + 'static,
    Fut: Future<Output = Result<A, E>> + 'static,
  {
    Self::from_instr(LInstr::Async(Box::new(move |env, fiber| {
      let fut = f(Ctx::new(downcast_env_arc::<R>(env), Arc::clone(fiber)));
      Box::pin(async move { lerase_exit(fut.await.map_err(Cause::Fail)) })
    })))
  }

  /// Appends an erased continuation (pushes onto an existing `Seq`).
  fn seq(self, k: LCont) -> LInstr {
    match self.node {
      LNode::Op(LInstr::Seq(mut body), _) => {
        body.1.push(k);
        LInstr::Seq(body)
      }
      node => LInstr::Seq(Box::new((Self { node }.into_instr(), vec![k]))),
    }
  }

  /// Transforms the success value (lazily: `f` runs when the effect runs).
  pub fn map<B: 'static>(self, f: impl FnOnce(A) -> B + 'static) -> LocalEffect<B, E, R> {
    match self.node {
      LNode::Ready(r) => LocalEffect::step(move |_, _| LInstr::Pure(lerase_exit(r.map(f)))),
      node => LocalEffect::from_instr(Self { node }.seq(Box::new(move |v| {
        LInstr::Pure(Ok(lerase(f(lunbox::<A>(v)))))
      }))),
    }
  }

  /// Sequences `f` after this effect's success.
  pub fn flat_map<B: 'static>(
    self,
    f: impl FnOnce(A) -> LocalEffect<B, E, R> + 'static,
  ) -> LocalEffect<B, E, R> {
    match self.node {
      LNode::Ready(Ok(a)) => LocalEffect::step(move |_, _| f(a).into_instr()),
      LNode::Ready(Err(c)) => LocalEffect::ready(Err(c)),
      node => LocalEffect::from_instr(
        Self { node }.seq(Box::new(move |v| f(lunbox::<A>(v)).into_instr())),
      ),
    }
  }

  /// Sequences `next` after this effect, discarding this effect's value.
  pub fn then<B: 'static>(self, next: LocalEffect<B, E, R>) -> LocalEffect<B, E, R> {
    self.flat_map(move |_| next)
  }

  /// Runs both effects in order and pairs their values.
  pub fn zip<B: 'static>(self, other: LocalEffect<B, E, R>) -> LocalEffect<(A, B), E, R> {
    self.flat_map(move |a| other.map(move |b| (a, b)))
  }

  /// Transforms the typed error (defects and interrupts pass through). As for
  /// [`Effect::map_err`], only the first typed failure of a composite cause is mapped.
  pub fn map_err<E2: 'static>(self, f: impl FnOnce(E) -> E2 + 'static) -> LocalEffect<A, E2, R> {
    match self.node {
      LNode::Ready(r) => {
        LocalEffect::step(move |_, _| LInstr::Pure(lerase_exit(r.map_err(|c| map_fail_once(c, f)))))
      }
      node => LocalEffect::from_instr(LInstr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(move |exit| match exit {
          Ok(v) => LInstr::Pure(Ok(v)),
          Err(c) => LInstr::Pure(Err(map_fail_once(c, |e| lerase(f(lunbox::<E>(e)))))),
        }),
      )),
    }
  }

  /// Recovers from a typed failure with `f` (defects and interrupts pass through).
  pub fn catch<E2: 'static>(
    self,
    f: impl FnOnce(E) -> LocalEffect<A, E2, R> + 'static,
  ) -> LocalEffect<A, E2, R> {
    match self.node {
      LNode::Ready(Ok(a)) => LocalEffect::ready(Ok(a)),
      LNode::Ready(Err(c)) => match split_failure::<E, E2>(c) {
        Ok(e) => LocalEffect::step(move |_, _| f(e).into_instr()),
        Err(c) => LocalEffect::ready(Err(c)),
      },
      node => LocalEffect::from_instr(LInstr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(move |exit| match exit {
          Ok(v) => LInstr::Pure(Ok(v)),
          Err(c) => match split_failure::<LBox, LBox>(c) {
            Ok(e) => f(lunbox::<E>(e)).into_instr(),
            Err(c) => LInstr::Pure(Err(c)),
          },
        }),
      )),
    }
  }

  /// Observes the full outcome as a value: never fails with `E2`.
  pub fn exit<E2: 'static>(self) -> LocalEffect<Exit<A, E>, E2, R> {
    match self.node {
      LNode::Ready(r) => LocalEffect::ready(Ok(to_exit(r))),
      node => LocalEffect::from_instr(LInstr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(|exit| LInstr::Pure(Ok(lerase(to_exit(lunerase_exit::<A, E>(exit)))))),
      )),
    }
  }

  /// Runs `finalizer` after this effect on every outcome (success, failure, die, interrupt),
  /// uninterruptibly. The original outcome is kept; a finalizer defect is sequenced after it
  /// (`Cause::Then`), or replaces a success.
  pub fn ensuring(self, finalizer: LocalEffect<(), Never, R>) -> Self {
    // `uninterruptible_mask(|restore| restore(self).fold(finalizer))`: the body keeps the
    // caller's mask, and the handler (and so the finalizer) starts inside the outer mask, so an
    // interrupt cannot skip it.
    Self::step(move |_, fiber| {
      let restore = fiber.mask();
      let handler: LHandler = Box::new(move |exit| {
        LInstr::Fold(
          Box::new(finalizer.into_instr()),
          Box::new(move |fin| match (exit, fin) {
            (exit, Ok(_)) => LInstr::Pure(exit),
            (Ok(_), Err(fc)) => LInstr::Pure(Err(fc)),
            (Err(c), Err(fc)) => LInstr::Pure(Err(Cause::Then(Box::new(c), Box::new(fc)))),
          }),
        )
      });
      LInstr::Mask(
        Box::new(LInstr::Fold(
          Box::new(LInstr::Mask(Box::new(self.into_instr()), Some(restore))),
          handler,
        )),
        None,
      )
    })
  }

  /// Runs this effect uninterruptibly: an interrupt requested inside is honoured when it exits.
  pub fn uninterruptible(self) -> Self {
    LocalEffect::from_instr(LInstr::Mask(Box::new(self.into_instr()), None))
  }

  /// Supplies the capability `T`, removing it from `R` (the `LocalEffect` twin of
  /// `Effect::provide`).
  pub fn provide<T, I>(self, t: T) -> LocalEffect<A, E, R::Rest>
  where
    T: Send + Sync + 'static,
    R: Plucker<T, I>,
  {
    LocalEffect::from_instr(LInstr::Provide(
      Box::new(self.into_instr()),
      Box::new(move |env| {
        let rest = downcast_env::<R::Rest>(env).clone();
        Arc::new(R::plant(Arc::new(t), rest)) as AnyEnv
      }),
    ))
  }

  /// Supplies the whole environment.
  pub fn provide_env(self, env: R) -> LocalEffect<A, E, HNil> {
    LocalEffect::from_instr(LInstr::Provide(
      Box::new(self.into_instr()),
      Box::new(move |_| Arc::new(env) as AnyEnv),
    ))
  }

  /// Drives this effect under `ctx`'s env and fiber, as the returned future's own local runloop
  /// (crate hook for `effect_local_next!` binds).
  pub(crate) fn run_in(self, ctx: Ctx<R>) -> LocalRunFuture<A, E> {
    let state = match self.node {
      LNode::Ready(r) => LRunState::Ready(Some(r)),
      LNode::Op(instr, _) => LRunState::Running(LocalRunloop {
        cur: Some(instr),
        fut: None,
        stack: Vec::new(),
        env: ctx.env as AnyEnv,
        fiber: ctx.fiber,
      }),
    };
    LocalRunFuture {
      state,
      _out: PhantomData,
    }
  }
}

impl<A: 'static, R: Send + Sync + 'static> LocalEffect<A, Never, R> {
  /// An infallible effect under any error type.
  pub fn widen<E: 'static>(self) -> LocalEffect<A, E, R> {
    self.map_err(|n| match n {})
  }
}

impl<E: 'static, R: Send + Sync + 'static> LocalEffect<(), E, R> {
  /// Suspends the fiber for `d` on its executor's timer.
  pub fn sleep(d: Duration) -> Self {
    Self::from_instr(LInstr::Async(Box::new(move |_, fiber| {
      let s = fiber.executor().sleep(d);
      Box::pin(async move {
        s.await;
        Ok(lerase(()))
      })
    })))
  }

  /// Yields once to whoever polls this effect (woken immediately, re-polled).
  pub fn yield_fiber() -> Self {
    Self::from_instr(LInstr::Async(Box::new(|_, _| {
      let mut yielded = false;
      Box::pin(std::future::poll_fn(move |cx| {
        if yielded {
          Poll::Ready(Ok(lerase(())))
        } else {
          yielded = true;
          cx.waker().wake_by_ref();
          Poll::Pending
        }
      }))
    })))
  }
}

/// The future of a `LocalEffect` driven on a given fiber: see [`LocalEffect::run_on`].
#[must_use = "futures do nothing unless polled"]
pub struct LocalRunFuture<A, E> {
  state: LRunState<A, E>,
  _out: PhantomData<fn() -> (A, E)>,
}

enum LRunState<A, E> {
  Ready(Option<Result<A, Cause<E>>>),
  Running(LocalRunloop),
}

// Never pin-projected: the ready value is moved out by `Option::take`; `LocalRunloop` is `Unpin`.
impl<A, E> Unpin for LocalRunFuture<A, E> {}

impl<A, E> std::fmt::Debug for LocalRunFuture<A, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self.state {
      LRunState::Ready(_) => "LocalRunFuture::Ready",
      LRunState::Running(_) => "LocalRunFuture::Running",
    })
  }
}

impl<A: 'static, E: 'static> Future for LocalRunFuture<A, E> {
  type Output = Exit<A, E>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    match &mut self.get_mut().state {
      LRunState::Ready(r) => Poll::Ready(to_exit(
        r.take()
          .expect("next: LocalRunFuture polled after completion"),
      )),
      LRunState::Running(rl) => Pin::new(rl)
        .poll(cx)
        .map(|exit| to_exit(lunerase_exit::<A, E>(exit))),
    }
  }
}

impl<A: 'static, E: 'static> LocalEffect<A, E, HNil> {
  /// Drives this effect on `fiber`, as the returned (`!Send`) future's own runloop. Interrupt it
  /// with `fiber.request_interrupt()` from any thread.
  ///
  /// Like [`Effect::run_on`], this does not await children forked under `fiber` (by a nested
  /// `Send` effect); [`run_fiber`](Self::run_fiber) does.
  pub fn run_on(self, fiber: Arc<FiberContext>) -> LocalRunFuture<A, E> {
    self.run_in(Ctx::new(Arc::new(HNil), fiber))
  }

  /// Drives this effect as the root fiber `fiber`, then interrupts and awaits `fiber`'s live
  /// children and marks it done (the `LocalEffect` twin of [`Effect::run_fiber`]).
  pub fn run_fiber(self, fiber: Arc<FiberContext>) -> impl Future<Output = Exit<A, E>> {
    let run = self.run_on(Arc::clone(&fiber));
    async move {
      let exit = run.await;
      finish_children(&fiber).await;
      fiber.mark_done();
      exit
    }
  }

  /// Runs this effect on the calling thread, parking it between wake-ups, as a root fiber on the
  /// default runtime's executor (timers, nested `Send` effects).
  ///
  /// # Errors
  ///
  /// [`BlockOnInAsync`] inside an async context; nothing is run.
  pub fn try_block_on(self) -> Result<Exit<A, E>, BlockOnInAsync> {
    if in_async_context() {
      return Err(BlockOnInAsync);
    }
    Ok(match self.node {
      LNode::Ready(r) => to_exit(r),
      node => {
        let exec = Arc::clone(Runtime::global().executor());
        park_on(Self { node }.run_fiber(FiberContext::new_root(exec)))
      }
    })
  }

  /// [`try_block_on`](Self::try_block_on), with [`BlockOnInAsync`] reported as a defect.
  pub fn block_on(self) -> Exit<A, E> {
    self
      .try_block_on()
      .unwrap_or_else(|e| Exit::Failure(Cause::Die(Defect::new(e.to_string()))))
  }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The one-shot slot a `run_on_local` worker completes.
struct LocalSlot<A, E> {
  exit: Option<Exit<A, E>>,
  waker: Option<Waker>,
}

/// Awaits a `run_on_local` worker; dropping it before completion interrupts the worker's fiber.
struct JoinLocal<A, E> {
  slot: Arc<Mutex<LocalSlot<A, E>>>,
  fiber: Arc<FiberContext>,
  done: bool,
}

impl<A, E> Future for JoinLocal<A, E> {
  type Output = Exit<A, E>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    let mut slot = lock(&self.slot);
    match slot.exit.take() {
      Some(exit) => {
        drop(slot);
        self.done = true;
        Poll::Ready(exit)
      }
      None => {
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
      }
    }
  }
}

impl<A, E> Drop for JoinLocal<A, E> {
  fn drop(&mut self) {
    if !self.done {
      self.fiber.request_interrupt();
    }
  }
}

impl<A: Send + 'static, E: Send + 'static> LocalEffect<A, E, HNil> {
  /// Starts this effect now on `exec`'s `spawn_local` worker (its own root fiber) and returns a
  /// `Send` [`Effect`] that awaits its exit: the only `LocalEffect → Effect` direction (ADR 0011).
  ///
  /// Interrupting the fiber that awaits the returned effect, or dropping that effect unrun,
  /// interrupts the local fiber.
  ///
  /// # Errors
  ///
  /// [`NotLocal`] when `exec` has no local worker (`TokioExecutor` today); the effect is
  /// dropped unrun.
  pub fn run_on_local(self, exec: &Arc<dyn Executor>) -> Result<Effect<A, E, HNil>, NotLocal> {
    let slot = Arc::new(Mutex::new(LocalSlot {
      exit: None,
      waker: None,
    }));
    let fiber = FiberContext::new_root(Arc::clone(exec));
    let run = self.run_fiber(Arc::clone(&fiber));
    let done = Arc::clone(&slot);
    exec.spawn_local(Box::pin(async move {
      let exit = run.await;
      let waker = {
        let mut s = lock(&done);
        s.exit = Some(exit);
        s.waker.take()
      };
      if let Some(w) = waker {
        w.wake();
      }
    }))?;
    let join = JoinLocal {
      slot,
      fiber,
      done: false,
    };
    Ok(Effect::from_instr(Instr::Future(Box::pin(async move {
      erase_exit(from_exit(join.await))
    }))))
  }
}

/// A nested runloop on the enclosing runloop's fiber that holds one mask level while it is
/// suspended (the local twin of `BindFuture`'s hold, used by `~` binds in `effect_local_next!`
/// and by [`Effect::local`]).
///
/// Without it, an interrupt of a suspended nested runloop is seen first by the enclosing
/// runloop, which drops the body's future, and the nested runloop with it, so the sub-effect's
/// finalizers never run. Holding the mask keeps the enclosing runloop polling; the level is
/// released before every poll, so the nested runloop sees the interrupt itself, unwinds its
/// frames (running finalizers) and reports `Interrupt`.
pub(crate) struct HoldMask<F> {
  fut: F,
  fiber: Arc<FiberContext>,
  /// The same hold as `BindFuture`'s, so local and `Send` binds suspended side by side under a
  /// `join` do not hold an interrupt off each other.
  hold: BindHold,
}

impl<F> HoldMask<F> {
  pub(crate) fn new(fut: F, fiber: Arc<FiberContext>) -> Self {
    Self {
      fut,
      fiber,
      hold: BindHold::default(),
    }
  }
}

impl<F: Future + Unpin> Future for HoldMask<F> {
  type Output = F::Output;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
    let this = &mut *self;
    let outer = this.hold.lift(&this.fiber);
    let polled = Pin::new(&mut this.fut).poll(cx);
    this.hold.settle(&this.fiber, outer, polled.is_pending());
    polled
  }
}

impl<F> Drop for HoldMask<F> {
  fn drop(&mut self) {
    // Dropped while suspended (the body was abandoned): give the hold back.
    self.hold.release(&self.fiber);
  }
}

/// What `~e` binds inside `effect_local_next!`: a [`LocalEffect`] or a `Send` [`Effect`] over
/// the same environment.
pub trait IntoLocal<A, E, R> {
  /// The `!Send` view of `self` (free for an `Effect`).
  fn into_local(self) -> LocalEffect<A, E, R>;
}

impl<A, E, R> IntoLocal<A, E, R> for LocalEffect<A, E, R> {
  fn into_local(self) -> Self {
    self
  }
}

impl<A, E, R> IntoLocal<A, E, R> for Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  fn into_local(self) -> LocalEffect<A, E, R> {
    self.local()
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// The `!Send` view of this effect: free for a ready value; otherwise the `Send` program runs
  /// as a nested runloop on the local effect's fiber and env (interrupts and masks carry over).
  pub fn local(self) -> LocalEffect<A, E, R> {
    match self.node {
      Node::Pure(r) => LocalEffect::ready(r),
      node => {
        let instr = Effect { node }.into_instr();
        LocalEffect::from_instr(LInstr::Async(Box::new(move |env, fiber| {
          let rl = Runloop::new(instr, Arc::clone(env), Arc::clone(fiber));
          let rl = HoldMask::new(rl, Arc::clone(fiber));
          Box::pin(async move { lerase_exit(unerase_exit::<A, E>(rl.await)) })
        })))
      }
    }
  }
}
