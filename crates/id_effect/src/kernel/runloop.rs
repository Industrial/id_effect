//! The erased runloop: `Instr`, `Frame`, `MaskOp` and `Runloop` (frame stack, budget, interrupt
//! checks).
//!
//! Design A §1.1/§1.3: an explicit continuation stack over erased instructions, interrupt checks
//! at every instruction boundary and before every poll of an in-flight future, mask regions, and
//! a cooperative yield budget.
//!
//! `k-runloop-opt` (`docs/benchmarks/k-runloop-opt.md` has the ablation):
//!
//! - **One panic boundary per poll** (`Runloop::poll`) instead of a `catch_unwind` per step. A
//!   panic re-enters the loop as `Die` with every pending frame still on the stack, so `Fold`
//!   handlers and finalizers run exactly as for any other failure.
//! - **Bind fast path** (`Runloop::feed`): a success fed to a `FlatMap` frame, whose
//!   continuation returns a ready value or a future that is ready on its first poll, goes on to
//!   the next frame without a trip through the generic instruction loop. Every enum re-wrap is a
//!   copy, and a copy right after the callee's stores stalls store-to-load forwarding; that, not
//!   `catch_unwind` or the interrupt loads, was most of the per-bind cost.
//! - **A `Seq` holds frames**: an empty stack adopts its buffer instead of copying it.
//! - **A fiber-free prefix** (`settle_instr`): `block_on` and `~` binds run ready values and
//!   `map`/`flat_map` frames before any fiber, runloop or executor exists.
//!
//! The interrupt points are unchanged: no continuation runs after an unmasked interrupt is
//! observed, and a running fiber observes one within one instruction.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::anybox::{AnyBox, AnyEnv, AnyExit, hnil_env, unerase_exit};
use super::cause::{cause_from_panic, to_exit};
use super::effect::{Effect, Node};
use crate::capability::HNil;
use crate::failure::{Cause, Exit};
use crate::runtime::{BoxFuture, FiberContext};

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
  /// An instruction followed by continuation frames (see [`SeqBody`]).
  Seq(Box<SeqBody>),
  /// Runs on success *and* failure (typed `catch`, `exit`, `map_err`, finalizers).
  Fold(Box<Instr>, Handler),
  /// Run the inner instruction under a different env (`provide*`, `narrow`).
  Provide(Box<Instr>, ProvideFn),
  /// Run the inner instruction under a different mask.
  Mask(Box<Instr>, MaskOp),
}

/// The body of an [`Instr::Seq`]: `inner`, then `frames` in run order. Repeated `flat_map`/`map`
/// on an erased effect push onto `frames` instead of nesting a box per step, and a fresh runloop
/// adopts the buffer as its stack (see `push_seq`).
///
/// Boxed as one unit, so an `Effect` holding a `Seq` is a tag and a pointer: the builder moves
/// it by value at every `flat_map`, and a wide move of a just-written `Vec` stalls store-to-load
/// forwarding.
pub(crate) struct SeqBody {
  pub(crate) inner: Instr,
  pub(crate) frames: Vec<Frame>,
}

/// How an [`Instr::Mask`] region changes the fiber's mask.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MaskOp {
  /// Enter an uninterruptible region.
  Inc,
  /// Restore an earlier mask (`uninterruptible_mask`'s `restore`).
  Set(u32),
}

/// What the runloop pushes on its explicit stack.
pub(crate) enum Frame {
  FlatMap(Cont),
  Fold(Handler),
  Env(AnyEnv),
  Mask(u32),
}

/// Instructions per poll before the runloop yields to the executor (fairness).
const BUDGET: u32 = 2048;

/// Initial capacity of a `Seq`'s continuation list: a short chain never reallocates, a long one
/// grows by doubling.
const SEQ_CAPACITY: usize = 8;

/// A fresh `Seq` continuation list.
#[inline]
pub(crate) fn frame_vec() -> Vec<Frame> {
  Vec::with_capacity(SEQ_CAPACITY)
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

  /// Pops frames until a continuation consumes `exit` and returns its next instruction; once the
  /// stack is empty, `Instr::Pure(exit)` (the caller checks for an empty stack first).
  ///
  /// The continuation's instruction is returned as is, not wrapped (in a `Result`): every
  /// re-wrap of the enum is a copy, and copies right after the callee's stores stall
  /// store-to-load forwarding.
  #[inline(always)]
  fn unwind(&mut self, mut exit: AnyExit) -> Instr {
    while let Some(frame) = self.stack.pop() {
      match frame {
        Frame::FlatMap(k) => match exit {
          Ok(v) => return k(v),
          Err(c) => exit = Err(c),
        },
        Frame::Fold(h) => return h(exit),
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
    Instr::Pure(exit)
  }

  /// Pushes a frame.
  #[inline]
  fn push(&mut self, f: Frame) {
    self.stack.push(f);
  }

  /// Pushes a `Seq`'s continuations (in run order) so the first one is on top.
  #[inline]
  fn push_seq(&mut self, conts: Vec<Frame>) {
    push_seq(&mut self.stack, conts);
  }
}

/// `exit` with its success value rebuilt from its parts (see `Runloop::feed`).
#[inline(always)]
#[allow(
  clippy::needless_match,
  reason = "the variant-by-variant rebuild is the point: it avoids a whole-enum copy"
)]
fn take_value(exit: AnyExit) -> Result<AnyBox, Cause<AnyBox>> {
  match exit {
    Ok(AnyBox::Word(w)) => Ok(AnyBox::Word(w)),
    Ok(AnyBox::Unit) => Ok(AnyBox::Unit),
    Ok(AnyBox::Boxed(b)) => Ok(AnyBox::Boxed(b)),
    Err(c) => Err(c),
  }
}

/// Pushes `conts` (in run order) onto `stack` so the first one is on top. An empty stack adopts
/// the list's buffer instead of copying it into its own.
#[inline]
fn push_seq(stack: &mut Vec<Frame>, mut conts: Vec<Frame>) {
  if stack.is_empty() {
    conts.reverse();
    *stack = conts;
  } else {
    stack.extend(conts.drain(..).rev());
  }
}

impl Future for Runloop {
  type Output = AnyExit;

  /// One poll: [`drive`](Runloop::drive) under a single panic boundary.
  ///
  /// A panic in a user step (a continuation, handler, sync closure or future poll) unwinds out
  /// of `drive` to here. The instruction that panicked was already moved out of `cur` (or its
  /// future is dropped here), and every frame below it is still on the stack, so the panic
  /// re-enters the interpreter as `Die` (an `InterruptSignal` as `Interrupt`) and unwinds the
  /// frame stack as any failure does: `Fold` handlers and finalizers run.
  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<AnyExit> {
    let this = &mut *self;
    let mut budget = BUDGET;
    loop {
      match catch_unwind(AssertUnwindSafe(|| this.drive(cx, &mut budget))) {
        Ok(p) => return p,
        Err(payload) => {
          this.fut = None;
          this.cur = Some(Instr::Pure(Err(cause_from_panic(payload))));
        }
      }
    }
  }
}

impl Runloop {
  /// The interpreter loop (no panic boundary of its own: see [`Runloop::poll`]).
  ///
  /// The current instruction lives in a local while the loop runs (`cur` holds it only across a
  /// yield), and a fresh future is polled as soon as its instruction is reached, so a future that
  /// is ready at once feeds its exit straight into the next continuation.
  fn drive(&mut self, cx: &mut Context<'_>, budget: &mut u32) -> Poll<AnyExit> {
    let mut instr = match self.fut.as_mut() {
      Some(fut) => {
        let exit = if self.fiber.should_interrupt() {
          Err(Cause::Interrupt(self.fiber.id()))
        } else {
          match fut.as_mut().poll(cx) {
            Poll::Ready(exit) => exit,
            Poll::Pending => match self.suspend(cx) {
              Some(i) => return self.resume_after_interrupt(i, cx, budget),
              None => return Poll::Pending,
            },
          }
        };
        self.fut = None;
        Instr::Pure(exit)
      }
      None => match self.cur.take() {
        Some(i) => i,
        None => unreachable!("id_effect: runloop polled after completion"),
      },
    };
    loop {
      // Interrupt point: every instruction boundary, including a success value about to be fed
      // into a continuation (so no user closure runs after an unmasked interrupt).
      if !matches!(instr, Instr::Pure(Err(_))) && self.fiber.should_interrupt() {
        instr = Instr::Pure(Err(Cause::Interrupt(self.fiber.id())));
      }
      instr = match instr {
        Instr::Pure(exit) => {
          if self.stack.is_empty() {
            return Poll::Ready(exit);
          }
          match self.feed(exit, cx, budget) {
            Some(next) => next,
            None => return Poll::Pending,
          }
        }
        Instr::Sync(f) => f(&self.env),
        Instr::WithFiber(f) => f(&self.env, &self.fiber),
        Instr::Async(f) => {
          let fut = f(&self.env, &self.fiber);
          match self.start(fut, cx) {
            Some(next) => next,
            None => return Poll::Pending,
          }
        }
        Instr::Future(fut) => match self.start(fut, cx) {
          Some(next) => next,
          None => return Poll::Pending,
        },
        Instr::FlatMap(inner, k) => {
          self.push(Frame::FlatMap(k));
          *inner
        }
        Instr::Seq(body) => {
          let SeqBody { inner, frames } = *body;
          self.push_seq(frames);
          inner
        }
        Instr::Fold(inner, h) => {
          self.push(Frame::Fold(h));
          *inner
        }
        Instr::Provide(inner, f) => {
          let new_env = f(&self.env);
          let old = std::mem::replace(&mut self.env, new_env);
          self.push(Frame::Env(old));
          *inner
        }
        Instr::Mask(inner, op) => {
          let old = self.fiber.mask();
          self.push(Frame::Mask(old));
          self.fiber.set_mask(match op {
            MaskOp::Inc => old + 1,
            MaskOp::Set(m) => m,
          });
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

  /// The bind fast path: feeds a success into the `FlatMap` frame on top, and while the
  /// continuation returns a ready value or a future that is ready at once, keeps feeding the
  /// next frame without going back through the generic instruction loop. Anything else (a
  /// failure, another frame kind, another instruction) returns to the loop as `Some(instr)`;
  /// `None` means the runloop is suspended (or yielded on the budget) and `poll` returns
  /// `Pending`.
  ///
  /// It keeps every interrupt point of the loop: before each continuation, and before the first
  /// poll of each future.
  ///
  /// The carried value is rebuilt variant by variant from each result (`take_value`) instead of
  /// being moved as a whole: a whole-enum move right after the callee wrote it is a wide load
  /// over narrower stores, which stalls store-to-load forwarding.
  #[inline(always)]
  fn feed(&mut self, exit: AnyExit, cx: &mut Context<'_>, budget: &mut u32) -> Option<Instr> {
    let mut v = match (exit, self.stack.last()) {
      (Ok(v), Some(Frame::FlatMap(_))) => v,
      (exit, _) => return Some(self.unwind(exit)),
    };
    loop {
      let Some(Frame::FlatMap(k)) = self.stack.pop() else {
        unreachable!("id_effect: checked FlatMap frame (kernel bug)")
      };
      let next = match k(v) {
        Instr::Pure(e) => take_value(e),
        Instr::Future(mut fut) => {
          if self.fiber.should_interrupt() {
            Err(Cause::Interrupt(self.fiber.id()))
          } else {
            match fut.as_mut().poll(cx) {
              Poll::Ready(e) => take_value(e),
              Poll::Pending => {
                self.fut = Some(fut);
                return self.suspend(cx);
              }
            }
          }
        }
        other => return Some(other),
      };
      v = match next {
        Ok(v) if !self.fiber.should_interrupt() => v,
        Ok(_) => return Some(self.unwind(Err(Cause::Interrupt(self.fiber.id())))),
        Err(c) => return Some(self.unwind(Err(c))),
      };
      *budget -= 1;
      if *budget == 0 {
        self.cur = Some(Instr::Pure(Ok(v)));
        cx.waker().wake_by_ref();
        return None;
      }
      if !matches!(self.stack.last(), Some(Frame::FlatMap(_))) {
        return Some(self.unwind(Ok(v)));
      }
    }
  }

  /// Polls a fresh future once: its exit as the next instruction, or `None` once it is stored
  /// as the suspended future.
  #[inline]
  fn start(&mut self, mut fut: BoxFuture<'static, AnyExit>, cx: &mut Context<'_>) -> Option<Instr> {
    match fut.as_mut().poll(cx) {
      Poll::Ready(exit) => Some(Instr::Pure(exit)),
      Poll::Pending => {
        self.fut = Some(fut);
        self.suspend(cx)
      }
    }
  }

  /// The suspended future returned `Pending`: registers the waker, then re-checks the interrupt
  /// flag (a request between the poll and the registration would otherwise be missed). `Some`
  /// is the interrupt to continue with (the future is dropped), `None` means stay suspended.
  fn suspend(&mut self, cx: &mut Context<'_>) -> Option<Instr> {
    self.fiber.register_waker(cx.waker());
    if self.fiber.should_interrupt() {
      self.fut = None;
      Some(Instr::Pure(Err(Cause::Interrupt(self.fiber.id()))))
    } else {
      None
    }
  }

  /// Continues with `instr` after an interrupt was observed on a suspended future.
  #[cold]
  fn resume_after_interrupt(
    &mut self,
    instr: Instr,
    cx: &mut Context<'_>,
    budget: &mut u32,
  ) -> Poll<AnyExit> {
    self.cur = Some(instr);
    self.drive(cx, budget)
  }
}

/// Runs the fiber-free prefix of `instr` under `env`: ready values, sync closures and
/// `Seq`/`FlatMap`/`Fold` frames, with no fiber, runloop or executor. `Ok` is the program's exit;
/// `Err` is the rest of the program from the first instruction that needs a fiber (async steps,
/// masks, `Provide`, fiber access), with the frames still pending folded back into it.
///
/// Nothing can interrupt a program that has no fiber yet, so this prefix has no interrupt
/// points. A panic in a step becomes `Die` and unwinds the pending frames as in the runloop
/// (`Fold` handlers run).
pub(crate) fn settle_instr(instr: Instr, env: &AnyEnv) -> Result<AnyExit, Instr> {
  let mut cur = Some(instr);
  let mut stack: Vec<Frame> = Vec::new();
  let stopped = loop {
    match catch_unwind(AssertUnwindSafe(|| settle_loop(&mut cur, &mut stack, env))) {
      Ok(Some(exit)) => return Ok(exit),
      Ok(None) => break cur.take(),
      Err(payload) => cur = Some(Instr::Pure(Err(cause_from_panic(payload)))),
    }
  };
  let Some(rest) = stopped else {
    unreachable!("id_effect: settle stopped without an instruction (kernel bug)")
  };
  if stack.is_empty() {
    return Err(rest);
  }
  // Back in run order: the runloop pushes these frames again (adopting the buffer).
  stack.reverse();
  Err(Instr::Seq(Box::new(SeqBody {
    inner: rest,
    frames: stack,
  })))
}

/// The loop of [`settle_instr`]: `Some(exit)` when the program finished, `None` with the
/// stopping instruction left in `cur`.
fn settle_loop(cur: &mut Option<Instr>, stack: &mut Vec<Frame>, env: &AnyEnv) -> Option<AnyExit> {
  let mut instr = cur.take()?;
  loop {
    instr = match instr {
      Instr::Pure(mut exit) => loop {
        match stack.pop() {
          None => return Some(exit),
          Some(Frame::FlatMap(k)) => match exit {
            Ok(v) => break k(v),
            Err(c) => exit = Err(c),
          },
          Some(Frame::Fold(h)) => break h(exit),
          Some(Frame::Env(_) | Frame::Mask(_)) => {
            unreachable!("id_effect: settle pushed an env or mask frame (kernel bug)")
          }
        }
      },
      Instr::Sync(f) => f(env),
      Instr::FlatMap(inner, k) => {
        stack.push(Frame::FlatMap(k));
        *inner
      }
      Instr::Seq(body) => {
        let SeqBody { inner, frames } = *body;
        push_seq(stack, frames);
        inner
      }
      Instr::Fold(inner, h) => {
        stack.push(Frame::Fold(h));
        *inner
      }
      other => {
        *cur = Some(other);
        return None;
      }
    };
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
        r.take()
          .expect("id_effect: RunFuture polled after completion"),
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
        Arc::clone(hnil_env()),
        fiber,
      )),
    };
    RunFuture { state }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::{Duration, Instant};

  use super::*;
  use crate::kernel::{erase, unbox};
  use crate::runtime::{Executor, LocalBoxFuture, NotLocal};

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
    let region = Instr::Seq(Box::new(SeqBody {
      inner: Instr::Sync(Box::new(move |_| {
        f2.request_interrupt();
        Instr::Pure(Ok(erase(0_u32)))
      })),
      frames: vec![Frame::FlatMap(step(&inside)), Frame::FlatMap(step(&inside))],
    }));
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

  // ── k-runloop-opt: one panic boundary per poll ─────────────────────────────────────────────

  /// `Fold(handler) <- FlatMap(k) <- head`: `k` panics with a `String` payload; `handler` counts
  /// its runs and recovers to 7, and a continuation above the fold adds 1.
  fn panic_under_a_fold(
    head: Instr,
    handled: &Arc<AtomicUsize>,
    seen: &Arc<Mutex<Option<String>>>,
  ) -> Instr {
    let (h, s) = (Arc::clone(handled), Arc::clone(seen));
    let body = Instr::FlatMap(
      Box::new(head),
      Box::new(|_| -> Instr { std::panic::panic_any(String::from("step boom")) }),
    );
    let folded = Instr::Fold(
      Box::new(body),
      Box::new(move |exit| {
        h.fetch_add(1, Ordering::SeqCst);
        if let Err(Cause::Die(d)) = &exit {
          *s.lock().expect("lock") = d.with_payload(|p: &String| p.clone());
        }
        Instr::Pure(Ok(erase(7_u32)))
      }),
    );
    Instr::FlatMap(
      Box::new(folded),
      Box::new(|v| Instr::Pure(Ok(erase(unbox::<u32>(v) + 1)))),
    )
  }

  #[test]
  fn a_panicking_step_is_die_with_its_payload_and_the_frames_below_still_run() {
    let heads: [fn() -> Instr; 2] = [
      // after a ready value (fiber-free prefix in `settle_instr`, then in the runloop) ...
      || Instr::Pure(Ok(erase(1_u32))),
      // ... and after an async step (the bind fast path)
      || Instr::Future(Box::pin(async { Ok(erase(1_u32)) })),
    ];
    for head in heads {
      let fiber = FiberContext::new_root(Arc::new(NoExec));
      let (handled, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
      let exit = run(panic_under_a_fold(head(), &handled, &seen), &fiber);
      assert_eq!(
        unbox::<u32>(exit.ok().expect("recovered")),
        8,
        "the runloop went on"
      );
      assert_eq!(
        handled.load(Ordering::SeqCst),
        1,
        "the fold handler ran once"
      );
      assert_eq!(seen.lock().expect("lock").as_deref(), Some("step boom"));

      let (handled, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
      match settle_instr(panic_under_a_fold(head(), &handled, &seen), hnil_env()) {
        Ok(exit) => assert_eq!(unbox::<u32>(exit.ok().expect("recovered")), 8),
        Err(rest) => {
          let exit = run(rest, &fiber);
          assert_eq!(unbox::<u32>(exit.ok().expect("recovered")), 8);
        }
      }
      assert_eq!(handled.load(Ordering::SeqCst), 1);
      assert_eq!(seen.lock().expect("lock").as_deref(), Some("step boom"));
    }
  }

  #[test]
  fn a_panicking_future_is_die_and_unwinds_the_stack() {
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let (handled, seen) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
    let (h, s) = (Arc::clone(&handled), Arc::clone(&seen));
    let program = Instr::Fold(
      Box::new(Instr::FlatMap(
        Box::new(Instr::Future(Box::pin(async {
          std::panic::panic_any(String::from("poll boom"))
        }))),
        Box::new(|_| -> Instr { unreachable!("no continuation after a panicking poll") }),
      )),
      Box::new(move |exit| {
        h.fetch_add(1, Ordering::SeqCst);
        if let Err(Cause::Die(d)) = &exit {
          *s.lock().expect("lock") = d.with_payload(|p: &String| p.clone());
        }
        Instr::Pure(exit)
      }),
    );
    let exit = run(program, &fiber);
    assert!(matches!(exit, Err(Cause::Die(_))));
    assert_eq!(handled.load(Ordering::SeqCst), 1);
    assert_eq!(seen.lock().expect("lock").as_deref(), Some("poll boom"));
  }

  #[test]
  fn an_interrupt_stops_a_long_ready_bind_chain_at_the_next_continuation() {
    // 10k binds through the fast path; step 5000 interrupts its own fiber.
    let fiber = FiberContext::new_root(Arc::new(NoExec));
    let ran = Arc::new(AtomicUsize::new(0));
    let frames = (0..10_000_u32)
      .map(|i| {
        let (ran, f) = (Arc::clone(&ran), Arc::clone(&fiber));
        Frame::FlatMap(Box::new(move |v| {
          ran.fetch_add(1, Ordering::SeqCst);
          if i == 4_999 {
            f.request_interrupt();
          }
          let n = unbox::<u32>(v) + 1;
          Instr::Future(Box::pin(async move { Ok(erase(n)) }))
        }))
      })
      .collect();
    let program = Instr::Seq(Box::new(SeqBody {
      inner: Instr::Pure(Ok(erase(0_u32))),
      frames,
    }));
    let exit = run(program, &fiber);
    assert!(matches!(exit, Err(Cause::Interrupt(id)) if id == fiber.id()));
    assert_eq!(
      ran.load(Ordering::SeqCst),
      5_000,
      "no continuation after the interrupt"
    );
  }
}
