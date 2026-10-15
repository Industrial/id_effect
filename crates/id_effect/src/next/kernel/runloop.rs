//! The erased runloop: `Instr`, `Frame`, `MaskOp` and `Runloop` (frame stack, budget, interrupt
//! checks). Filled by `k-core-runloop`, optimised by `k-runloop-opt`.
//!
//! Design A §1.1/§1.3, ported from the spike as is: an explicit continuation stack over erased
//! instructions, interrupt checks at every instruction boundary and before every poll of an
//! in-flight future, mask regions, and a cooperative yield budget.
//!
//! Hooks for `k-runloop-opt` (SPIKE-REPORT §3 names them as the per-bind cost): every panic
//! boundary goes through [`guard`] / [`catch_step`], every interrupt check through
//! `FiberContext::should_interrupt`, and every erase/unbox through `anybox`. Moving
//! `catch_unwind` to the fiber edge, amortising the interrupt loads and adding a typed fast path
//! can each be done in one place.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::anybox::{AnyBox, AnyEnv, AnyExit, unerase_exit};
use super::cause::{cause_from_panic, to_exit};
use super::effect::{Effect, Node};
use crate::failure::{Cause, Exit};
use crate::next::capability::HNil;
use crate::next::runtime::{BoxFuture, FiberContext};

type Cont = Box<dyn FnOnce(AnyBox) -> Instr + Send>;
type Handler = Box<dyn FnOnce(AnyExit) -> Instr + Send>;
type SyncFn = Box<dyn FnOnce(&AnyEnv) -> Instr + Send>;
type WithFiberFn = Box<dyn FnOnce(&AnyEnv, &Arc<FiberContext>) -> Instr + Send>;
type AsyncFn = Box<dyn FnOnce(&AnyEnv, &Arc<FiberContext>) -> BoxFuture<'static, AnyExit> + Send>;
type ProvideFn = Box<dyn FnOnce(&AnyEnv) -> AnyEnv + Send>;

/// Instructions the runloop interprets. Typed `Effect`s erase into these only when they leave the
/// fused synchronous fast path (`Node::Pure` / `Node::Sync`).
#[allow(dead_code)] // WithFiber/FlatMap/Mask: built by k-fiber-tree; Provide: by k-typed-r
pub(crate) enum Instr {
  Pure(AnyExit),
  /// Compute the next instruction from the env (fused sync closures land here).
  Sync(SyncFn),
  /// Compute the next instruction from the env and the current fiber (fork, masks, finalizers:
  /// `k-fiber-tree` builds on this without touching the runloop).
  WithFiber(WithFiberFn),
  /// Suspend on a `'static` future built from the env (the env is borrowed synchronously only).
  Async(AsyncFn),
  /// A lazy future that needs neither env nor fiber (`from_async`).
  Future(BoxFuture<'static, AnyExit>),
  FlatMap(Box<Instr>, Cont),
  /// `inner` followed by `conts` in order: repeated `flat_map`/`map` on an erased effect push
  /// onto the vector instead of nesting a box per step.
  Seq(Box<Instr>, Vec<Cont>),
  /// Runs on success *and* failure (typed `catch`, `exit`, `map_err`, finalizers).
  Fold(Box<Instr>, Handler),
  /// Run the inner instruction under a different env (`provide*`, `narrow`).
  Provide(Box<Instr>, ProvideFn),
  /// Run the inner instruction under a different mask.
  Mask(Box<Instr>, MaskOp),
}

/// How an [`Instr::Mask`] region changes the fiber's mask.
#[allow(dead_code)] // built by k-fiber-tree (uninterruptible, uninterruptible_mask, ensuring)
#[derive(Clone, Copy, Debug)]
pub(crate) enum MaskOp {
  /// Enter an uninterruptible region.
  Inc,
  /// Restore an earlier mask (`uninterruptible_mask`'s `restore`).
  Set(u32),
}

/// What the runloop pushes on its explicit stack.
enum Frame {
  FlatMap(Cont),
  Fold(Handler),
  Env(AnyEnv),
  Mask(u32),
}

/// Instructions per poll before the runloop yields to the executor (fairness).
const BUDGET: u32 = 2048;

/// Runs one user step, turning a panic into a `Die` (or an `InterruptSignal` back into
/// `Interrupt`).
#[inline]
fn guard(f: impl FnOnce() -> Instr) -> Instr {
  match catch_step(f) {
    Ok(i) => i,
    Err(c) => Instr::Pure(Err(c)),
  }
}

/// `catch_unwind` around one step, with the payload converted to a cause.
#[inline]
fn catch_step<T>(f: impl FnOnce() -> T) -> Result<T, Cause<AnyBox>> {
  catch_unwind(AssertUnwindSafe(f)).map_err(cause_from_panic)
}

/// The erased interpreter: one per running fiber (and one per nested `~` bind).
pub(crate) struct Runloop {
  cur: Option<Instr>,
  fut: Option<BoxFuture<'static, AnyExit>>,
  stack: Vec<Frame>,
  env: AnyEnv,
  fiber: Arc<FiberContext>,
}

impl Runloop {
  pub(crate) fn new(instr: Instr, env: AnyEnv, fiber: Arc<FiberContext>) -> Self {
    Self {
      cur: Some(instr),
      fut: None,
      stack: Vec::new(),
      env,
      fiber,
    }
  }

  /// Pops frames until a continuation consumes `exit`; `Err(exit)` when the stack is empty.
  fn unwind(&mut self, mut exit: AnyExit) -> Result<Instr, AnyExit> {
    while let Some(frame) = self.stack.pop() {
      match frame {
        Frame::FlatMap(k) => match exit {
          Ok(v) => return Ok(guard(move || k(v))),
          Err(c) => exit = Err(c),
        },
        Frame::Fold(h) => return Ok(guard(move || h(exit))),
        Frame::Env(e) => self.env = e,
        Frame::Mask(m) => {
          self.fiber.set_mask(m);
          // Leaving a mask region is an interrupt point: an interrupt requested inside the
          // region is honoured here, before the value reaches the next continuation.
          if exit.is_ok() && self.fiber.should_interrupt() {
            exit = Err(Cause::Interrupt(self.fiber.id()));
          }
        }
      }
    }
    Err(exit)
  }
}

impl Future for Runloop {
  type Output = AnyExit;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    let this = &mut *self;
    let mut budget = BUDGET;
    loop {
      if let Some(fut) = this.fut.as_mut() {
        let exit = if this.fiber.should_interrupt() {
          Err(Cause::Interrupt(this.fiber.id()))
        } else {
          match catch_step(|| fut.as_mut().poll(cx)) {
            Ok(Poll::Pending) => {
              this.fiber.register_waker(cx.waker());
              if this.fiber.should_interrupt() {
                continue;
              }
              return Poll::Pending;
            }
            Ok(Poll::Ready(v)) => v,
            Err(c) => Err(c),
          }
        };
        this.fut = None;
        this.cur = Some(Instr::Pure(exit));
      }
      let Some(mut instr) = this.cur.take() else {
        unreachable!("next: runloop polled after completion");
      };
      // Interrupt point: every instruction boundary, including a success value about to be fed
      // into a continuation (so no user closure runs after an unmasked interrupt).
      if !matches!(instr, Instr::Pure(Err(_))) && this.fiber.should_interrupt() {
        instr = Instr::Pure(Err(Cause::Interrupt(this.fiber.id())));
      }
      match instr {
        Instr::Pure(exit) => match this.unwind(exit) {
          Ok(next) => this.cur = Some(next),
          Err(exit) => return Poll::Ready(exit),
        },
        Instr::Sync(f) => {
          let env = &this.env;
          this.cur = Some(guard(|| f(env)));
        }
        Instr::WithFiber(f) => {
          let (env, fiber) = (&this.env, &this.fiber);
          this.cur = Some(guard(|| f(env, fiber)));
        }
        Instr::Async(f) => {
          let (env, fiber) = (&this.env, &this.fiber);
          match catch_step(|| f(env, fiber)) {
            Ok(fut) => this.fut = Some(fut),
            Err(c) => this.cur = Some(Instr::Pure(Err(c))),
          }
        }
        Instr::Future(fut) => this.fut = Some(fut),
        Instr::FlatMap(inner, k) => {
          this.stack.push(Frame::FlatMap(k));
          this.cur = Some(*inner);
        }
        Instr::Seq(inner, conts) => {
          this
            .stack
            .extend(conts.into_iter().rev().map(Frame::FlatMap));
          this.cur = Some(*inner);
        }
        Instr::Fold(inner, h) => {
          this.stack.push(Frame::Fold(h));
          this.cur = Some(*inner);
        }
        Instr::Provide(inner, f) => {
          let env = &this.env;
          match catch_step(|| f(env)) {
            Ok(new_env) => {
              let old = std::mem::replace(&mut this.env, new_env);
              this.stack.push(Frame::Env(old));
              this.cur = Some(*inner);
            }
            Err(c) => this.cur = Some(Instr::Pure(Err(c))),
          }
        }
        Instr::Mask(inner, op) => {
          let old = this.fiber.mask();
          this.stack.push(Frame::Mask(old));
          this.fiber.set_mask(match op {
            MaskOp::Inc => old + 1,
            MaskOp::Set(m) => m,
          });
          this.cur = Some(*inner);
        }
      }
      budget -= 1;
      if budget == 0 {
        cx.waker().wake_by_ref();
        return Poll::Pending;
      }
    }
  }
}

/// The future of an `Effect<A, E, HNil>` driven on a given fiber: see [`Effect::run_on`].
///
/// A `Pure` effect stays typed and completes on the first poll without allocating; anything
/// else runs on a runloop owned by this future (no task is spawned).
#[must_use = "futures do nothing unless polled"]
pub struct RunFuture<A, E> {
  state: RunState<A, E>,
}

enum RunState<A, E> {
  Ready(Option<Result<A, Cause<E>>>),
  Running(Runloop),
}

// Never pin-projected: the typed value is moved out by `Option::take`, and `Runloop` is `Unpin`.
impl<A, E> Unpin for RunFuture<A, E> {}

impl<A, E> std::fmt::Debug for RunFuture<A, E> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self.state {
      RunState::Ready(_) => "RunFuture::Ready",
      RunState::Running(_) => "RunFuture::Running",
    })
  }
}

impl<A: 'static, E: 'static> Future for RunFuture<A, E> {
  type Output = Exit<A, E>;

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Exit<A, E>> {
    match &mut self.get_mut().state {
      RunState::Ready(r) => Poll::Ready(to_exit(
        r.take().expect("next: RunFuture polled after completion"),
      )),
      RunState::Running(rl) => Pin::new(rl)
        .poll(cx)
        .map(|exit| to_exit(unerase_exit::<A, E>(exit))),
    }
  }
}

impl<A: Send + 'static, E: Send + 'static> Effect<A, E, HNil> {
  /// Drives this effect on `fiber`, as the returned future's own runloop (no task is spawned).
  ///
  /// This is the building block of the run edges (`Runtime::run`, `block_on`, fibers). It does
  /// not await `fiber`'s children; `k-fiber-tree` adds that around it.
  pub fn run_on(self, fiber: Arc<FiberContext>) -> RunFuture<A, E> {
    let state = match self.node {
      Node::Pure(r) => RunState::Ready(Some(r)),
      node => RunState::Running(Runloop::new(
        Effect { node }.into_instr(),
        Arc::new(HNil) as AnyEnv,
        fiber,
      )),
    };
    RunFuture { state }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::{Duration, Instant};

  use super::*;
  use crate::next::kernel::{erase, unbox};
  use crate::next::runtime::{Executor, LocalBoxFuture, NotLocal};

  struct NoExec;

  impl Executor for NoExec {
    fn spawn(&self, _: BoxFuture<'static, ()>) {
      unreachable!("runloop unit tests spawn nothing")
    }
    fn spawn_local(&self, _: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal> {
      Err(NotLocal)
    }
    fn spawn_blocking(&self, _: Box<dyn FnOnce() + Send>) {
      unreachable!("runloop unit tests spawn nothing")
    }
    fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
      Box::pin(async {})
    }
    fn now(&self) -> Instant {
      Instant::now()
    }
  }

  fn run(instr: Instr, fiber: &Arc<FiberContext>) -> AnyExit {
    pollster::block_on(Runloop::new(
      instr,
      Arc::new(HNil) as AnyEnv,
      Arc::clone(fiber),
    ))
  }

  fn step(count: &Arc<AtomicUsize>) -> Cont {
    let count = Arc::clone(count);
    Box::new(move |v| {
      count.fetch_add(1, Ordering::SeqCst);
      Instr::Pure(Ok(erase(unbox::<u32>(v) + 1)))
    })
  }

  #[test]
  fn interrupt_inside_mask_is_deferred_until_the_region_exits() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let inside = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let f2 = Arc::clone(&fiber);
    // Mask(Inc){ request interrupt; +1; +1 } then +1 outside the region.
    let region = Instr::Seq(
      Box::new(Instr::Sync(Box::new(move |_| {
        f2.request_interrupt();
        Instr::Pure(Ok(erase(0_u32)))
      }))),
      vec![step(&inside), step(&inside)],
    );
    let program = Instr::FlatMap(
      Box::new(Instr::Mask(Box::new(region), MaskOp::Inc)),
      step(&after),
    );
    let exit = run(program, &fiber);
    assert_eq!(inside.load(Ordering::SeqCst), 2, "masked steps still run");
    assert_eq!(
      after.load(Ordering::SeqCst),
      0,
      "no step runs after the region exits"
    );
    assert!(matches!(exit, Err(Cause::Interrupt(id)) if id == fiber.id()));
    assert_eq!(fiber.mask(), 0);
  }

  #[test]
  fn restore_mask_reopens_interruption() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let ran = Arc::new(AtomicUsize::new(0));
    let f2 = Arc::clone(&fiber);
    let inner = Instr::Mask(
      Box::new(Instr::FlatMap(
        Box::new(Instr::Sync(Box::new(move |_| {
          f2.request_interrupt();
          Instr::Pure(Ok(erase(0_u32)))
        }))),
        step(&ran),
      )),
      MaskOp::Set(0),
    );
    let exit = run(Instr::Mask(Box::new(inner), MaskOp::Inc), &fiber);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(matches!(exit, Err(Cause::Interrupt(_))));
  }

  #[test]
  fn fold_sees_failures_and_provide_restores_env() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let program = Instr::Fold(
      Box::new(Instr::Provide(
        Box::new(Instr::Sync(Box::new(|env| {
          let v = *super::super::anybox::downcast_env::<u32>(env);
          Instr::Pure(Err(Cause::Fail(erase(v))))
        }))),
        Box::new(|_| Arc::new(41_u32) as AnyEnv),
      )),
      Box::new(|exit| match exit {
        Err(Cause::Fail(v)) => Instr::Sync(Box::new(move |env| {
          assert!(env.downcast_ref::<HNil>().is_some(), "outer env restored");
          Instr::Pure(Ok(erase(unbox::<u32>(v) + 1)))
        })),
        other => Instr::Pure(other),
      }),
    );
    let exit = run(program, &fiber);
    assert_eq!(unbox::<u32>(exit.ok().expect("recovered")), 42);
  }
}
