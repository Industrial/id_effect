//! The type-erased fiber runloop (design A §1.1/§1.3): an explicit continuation stack over
//! erased instructions, interrupt checks at every instruction boundary and before every poll of
//! an in-flight future, mask regions, and a cooperative yield budget.

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::cause::{Cause, cause_from_panic};
use crate::fiber::FiberContext;

/// An erased runloop value. Design A §5 R2 mitigation ("SmallBox"), in safe Rust: `()` and a
/// fixed set of word-sized primitives (recognised by `TypeId`) travel inline; every other type,
/// including small user structs, is boxed.
pub(crate) enum AnyBox {
  Unit,
  Word(u64),
  Boxed(Box<dyn Any + Send>),
}

pub(crate) fn erase<T: Send + 'static>(t: T) -> AnyBox {
  let mut slot = Some(t);
  let any: &mut dyn Any = &mut slot;
  if any.is::<Option<()>>() {
    return AnyBox::Unit;
  }
  macro_rules! word {
    ($($ty:ty),*) => {$(
      if let Some(s) = any.downcast_mut::<Option<$ty>>() {
        return AnyBox::Word(s.take().map_or(0, |v| v as u64));
      }
    )*};
  }
  word!(u64, usize, u32, i64, i32, bool);
  AnyBox::Boxed(Box::new(slot.take().expect("kernel_a: erase slot")))
}

pub(crate) fn unbox<T: 'static>(v: AnyBox) -> T {
  let mut out: Option<T> = None;
  let any: &mut dyn Any = &mut out;
  match v {
    AnyBox::Boxed(b) => match b.downcast::<T>() {
      Ok(t) => return *t,
      Err(_) => panic!("kernel_a: erased value type mismatch (kernel bug)"),
    },
    AnyBox::Unit => {
      if let Some(o) = any.downcast_mut::<Option<()>>() {
        *o = Some(());
      }
    }
    AnyBox::Word(w) => {
      macro_rules! word {
        ($($ty:ty),*) => {$(
          if let Some(o) = any.downcast_mut::<Option<$ty>>() {
            *o = Some(w as $ty);
          }
        )*};
      }
      word!(u64, usize, u32, i64, i32);
      if let Some(o) = any.downcast_mut::<Option<bool>>() {
        *o = Some(w != 0);
      }
    }
  }
  out.expect("kernel_a: erased value type mismatch (kernel bug)")
}

pub(crate) type AnyExit = Result<AnyBox, Cause<AnyBox>>;
pub(crate) type AnyEnv = Arc<dyn Any + Send + Sync>;

/// A `Send` boxed future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// A `!Send` boxed future (`LocalEffect`).
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

type Cont = Box<dyn FnOnce(AnyBox) -> Instr + Send>;
type Handler = Box<dyn FnOnce(AnyExit) -> Instr + Send>;
type WithFiberFn = Box<dyn FnOnce(&AnyEnv, &Arc<FiberContext>) -> Instr + Send>;
type AsyncFn = Box<dyn FnOnce(&AnyEnv, &Arc<FiberContext>) -> BoxFuture<'static, AnyExit> + Send>;

/// Instructions the runloop interprets. Typed `Effect`s erase into these only when they leave the
/// fused synchronous fast path (`Pure` / `Sync` in `effect.rs`).
pub(crate) enum Instr {
  Pure(AnyExit),
  /// Compute the next instruction from the env (fused sync closures land here).
  Sync(Box<dyn FnOnce(&AnyEnv) -> Instr + Send>),
  /// Compute the next instruction from the env and the current fiber (fork, masks).
  WithFiber(WithFiberFn),
  /// Suspend on a `'static` future built from the env (the env is borrowed synchronously only).
  Async(AsyncFn),
  /// A lazy future that needs neither env nor fiber (`from_async`).
  Future(BoxFuture<'static, AnyExit>),
  FlatMap(Box<Instr>, Cont),
  /// `inner` followed by `conts` in order: repeated `flat_map`/`map` on an erased effect push
  /// onto the vector instead of nesting a box per step.
  Seq(Box<Instr>, Vec<Cont>),
  /// Runs on success *and* failure (typed `catch`, `exit`, `ensuring`, finalizers).
  Fold(Box<Instr>, Handler),
  /// Run the inner instruction under a different env (`provide*`, `narrow`).
  Provide(Box<Instr>, Box<dyn FnOnce(&AnyEnv) -> AnyEnv + Send>),
  Mask(Box<Instr>, MaskOp),
}

#[derive(Clone, Copy)]
pub(crate) enum MaskOp {
  /// Enter an uninterruptible region.
  Inc,
  /// Restore an earlier mask (`uninterruptible_mask`'s `restore`).
  Set(u32),
}

enum Frame {
  FlatMap(Cont),
  Fold(Handler),
  Env(AnyEnv),
  Mask(u32),
}

/// Instructions per poll before the runloop yields to the executor (fairness).
const BUDGET: u32 = 2048;

pub(crate) struct Runloop {
  cur: Option<Instr>,
  fut: Option<BoxFuture<'static, AnyExit>>,
  stack: Vec<Frame>,
  env: AnyEnv,
  fiber: Arc<FiberContext>,
}

fn guard(f: impl FnOnce() -> Instr) -> Instr {
  match catch_unwind(AssertUnwindSafe(f)) {
    Ok(i) => i,
    Err(p) => Instr::Pure(Err(cause_from_panic(p))),
  }
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
        Frame::Mask(m) => self.fiber.set_mask(m),
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
          match catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(Poll::Pending) => {
              this.fiber.register_waker(cx.waker());
              if this.fiber.should_interrupt() {
                continue;
              }
              return Poll::Pending;
            }
            Ok(Poll::Ready(v)) => v,
            Err(p) => Err(cause_from_panic(p)),
          }
        };
        this.fut = None;
        this.cur = Some(Instr::Pure(exit));
      }
      let Some(mut instr) = this.cur.take() else {
        unreachable!("kernel_a: runloop polled after completion");
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
          match catch_unwind(AssertUnwindSafe(|| f(env, fiber))) {
            Ok(fut) => this.fut = Some(fut),
            Err(p) => this.cur = Some(Instr::Pure(Err(cause_from_panic(p)))),
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
          match catch_unwind(AssertUnwindSafe(|| f(env))) {
            Ok(new_env) => {
              let old = std::mem::replace(&mut this.env, new_env);
              this.stack.push(Frame::Env(old));
              this.cur = Some(*inner);
            }
            Err(p) => this.cur = Some(Instr::Pure(Err(cause_from_panic(p)))),
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
