//! The effect representation (design §1.1, option (c)): a step enum with an inline `Ready`
//! variant (no allocation for pure/ready binds), a boxed `Sync` closure, and boxed async steps.
//!
//! `Effect` (Send) and `LocalEffect` (!Send capable, §1.2) are generated from one `kernel_impl!`
//! template so the combinators exist once in source.
//!
//! Two async variants exist: `Async` is structural (combinators, uninterruptible waits) and
//! `Leaf` is a user-supplied suspension (`from_fn_async`, `effect!` bodies, `sleep`, `join`).
//! A `Leaf` that is still pending when its fiber has an unmasked interrupt request is dropped and
//! yields `Cause::Interrupt` (innermost pending leaf first, so enclosing finalizers still run).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub use crate::cause::{Cause, Exit, Never};

/// A boxed `Send` future (was `!Send` in 0.6, `kernel/effect.rs:73`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// A boxed future without the `Send` bound (for `LocalEffect`).
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Box a `Send` future (used by `effect!`).
#[inline]
pub fn box_future<'a, T, F: Future<Output = T> + Send + 'a>(f: F) -> BoxFuture<'a, T> {
  Box::pin(f)
}

/// Box a future without `Send` (used by `effect_local!`).
#[inline]
pub fn box_local_future<'a, T, F: Future<Output = T> + 'a>(f: F) -> LocalBoxFuture<'a, T> {
  Box::pin(f)
}

/// A `&'static mut ()` without allocating (a leaked ZST box), for running env-free effects.
#[inline]
pub(crate) fn unit_env() -> &'static mut () {
  Box::leak(Box::new(()))
}

macro_rules! kernel_impl {
  (
    $(#[$meta:meta])*
    effect = $Eff:ident, step = $Step:ident, run = $Run:ident, fut = $Fut:ident, boxf = $boxf:path,
    send = [$($send:tt)*], bound = [$($tb:tt)*]
  ) => {
    $(#[$meta])*
    #[must_use = "effects do nothing until they are run"]
    pub struct $Eff<A, E = Never, R = ()> {
      pub(crate) step: $Step<A, E, R>,
    }

    type SyncFn<A, E, R> = Box<dyn FnOnce(&mut R) -> Result<A, Cause<E>> $($send)*>;
    type AsyncFn<A, E, R> = Box<dyn for<'a> FnOnce(&'a mut R) -> $Fut<'a, Result<A, Cause<E>>> $($send)*>;

    pub(crate) enum $Step<A, E, R> {
      Ready(Result<A, Cause<E>>),
      Sync(SyncFn<A, E, R>),
      Async(AsyncFn<A, E, R>),
      Leaf(AsyncFn<A, E, R>),
    }

    /// The future that runs an effect against a borrowed environment. Ready and sync steps
    /// complete on first poll without allocating.
    pub enum $Run<'a, A, E, R> {
      #[doc(hidden)]
      Done(Option<Result<A, Cause<E>>>),
      #[doc(hidden)]
      Sync(Option<(SyncFn<A, E, R>, &'a mut R)>),
      #[doc(hidden)]
      Async($Fut<'a, Result<A, Cause<E>>>),
      #[doc(hidden)]
      Leaf($Fut<'a, Result<A, Cause<E>>>),
    }

    // Sound: no field is ever pinned structurally (values are moved out, boxes are `Unpin`).
    impl<A, E, R> Unpin for $Run<'_, A, E, R> {}

    impl<A, E, R> Future for $Run<'_, A, E, R> {
      type Output = Result<A, Cause<E>>;

      fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        match this {
          Self::Done(v) => Poll::Ready(v.take().expect("kernel_b: effect polled after completion")),
          Self::Sync(slot) => {
            let (f, r) = slot.take().expect("kernel_b: effect polled after completion");
            Poll::Ready(f(r))
          }
          Self::Async(f) => f.as_mut().poll(cx),
          Self::Leaf(f) => match f.as_mut().poll(cx) {
            Poll::Ready(v) => Poll::Ready(v),
            Poll::Pending => match $crate::fiber::interrupt_requested() {
              Some(id) => {
                // Drop the in-flight future (e.g. `Db::hang`) and surface the interrupt.
                *this = Self::Done(None);
                Poll::Ready(Err(Cause::Interrupt(id)))
              }
              None => Poll::Pending,
            },
          },
        }
      }
    }

    impl<A: $($tb)*, E: $($tb)*, R: $($tb)*> $Eff<A, E, R> {
      #[inline]
      pub fn succeed(a: A) -> Self {
        Self { step: $Step::Ready(Ok(a)) }
      }

      #[inline]
      pub fn fail(e: E) -> Self {
        Self { step: $Step::Ready(Err(Cause::Fail(e))) }
      }

      #[inline]
      pub fn fail_cause(c: Cause<E>) -> Self {
        Self { step: $Step::Ready(Err(c)) }
      }

      #[inline]
      pub fn from_result(r: Result<A, E>) -> Self {
        Self { step: $Step::Ready(r.map_err(Cause::Fail)) }
      }

      /// A synchronous step over the environment (not interruptible).
      pub fn sync<F>(f: F) -> Self
      where
        F: FnOnce(&mut R) -> Result<A, Cause<E>> $($send)* + 'static,
      {
        Self { step: $Step::Sync(Box::new(f)) }
      }

      /// A suspending leaf over the environment (what `effect!` expands to). Interruptible.
      pub fn from_fn_async<F>(f: F) -> Self
      where
        F: for<'a> FnOnce(&'a mut R) -> $Fut<'a, Result<A, Cause<E>>> $($send)* + 'static,
      {
        Self { step: $Step::Leaf(Box::new(f)) }
      }

      /// An env-free future as an interruptible leaf.
      pub fn from_async<F>(fut: F) -> Self
      where
        F: Future<Output = Result<A, E>> $($send)* + 'static,
      {
        Self::from_fn_async(move |_r| $boxf(async move { fut.await.map_err(Cause::Fail) }))
      }

      /// A structural (uninterruptible-while-pending) async step. Crate-internal.
      #[allow(dead_code)]
      pub(crate) fn structured<F>(f: F) -> Self
      where
        F: for<'a> FnOnce(&'a mut R) -> $Fut<'a, Result<A, Cause<E>>> $($send)* + 'static,
      {
        Self { step: $Step::Async(Box::new(f)) }
      }

      /// Run against a borrowed environment.
      #[inline]
      pub fn run_in(self, r: &mut R) -> $Run<'_, A, E, R> {
        match self.step {
          $Step::Ready(v) => $Run::Done(Some(v)),
          $Step::Sync(f) => $Run::Sync(Some((f, r))),
          $Step::Async(f) => $Run::Async(f(r)),
          $Step::Leaf(f) => $Run::Leaf(f(r)),
        }
      }

      /// `true` for an already-completed effect (`succeed`, `fail`, a bind of those).
      pub fn is_ready(&self) -> bool {
        matches!(self.step, $Step::Ready(_))
      }

      pub fn map<B: $($tb)*, F>(self, f: F) -> $Eff<B, E, R>
      where
        F: FnOnce(A) -> B $($send)* + 'static,
      {
        match self.step {
          $Step::Ready(Ok(a)) => $Eff { step: $Step::Ready(Ok(f(a))) },
          $Step::Ready(Err(c)) => $Eff { step: $Step::Ready(Err(c)) },
          $Step::Sync(g) => $Eff::sync(move |r| g(r).map(f)),
          step => $Eff::structured(move |r| {
            $boxf(async move { $Eff { step }.run_in(r).await.map(f) })
          }),
        }
      }

      pub fn flat_map<B: $($tb)*, F>(self, f: F) -> $Eff<B, E, R>
      where
        F: FnOnce(A) -> $Eff<B, E, R> $($send)* + 'static,
      {
        match self.step {
          // Ready fast path: the continuation runs eagerly, with no allocation (R1).
          $Step::Ready(Ok(a)) => f(a),
          $Step::Ready(Err(c)) => $Eff { step: $Step::Ready(Err(c)) },
          step => $Eff::structured(move |r| {
            $boxf(async move {
              let a = $Eff { step }.run_in(&mut *r).await?;
              if let Some(id) = $crate::fiber::interrupt_requested() {
                return Err(Cause::Interrupt(id));
              }
              f(a).run_in(r).await
            })
          }),
        }
      }

      pub fn map_error<E2: $($tb)*, F>(self, f: F) -> $Eff<A, E2, R>
      where
        F: FnOnce(E) -> E2 $($send)* + 'static,
      {
        match self.step {
          $Step::Ready(v) => $Eff { step: $Step::Ready(v.map_err(|c| c.map_fail(f))) },
          $Step::Sync(g) => $Eff::sync(move |r| g(r).map_err(|c| c.map_fail(f))),
          step => $Eff::structured(move |r| {
            $boxf(async move { $Eff { step }.run_in(r).await.map_err(|c| c.map_fail(f)) })
          }),
        }
      }

      /// Recover from a typed failure. Defects and interrupts pass through.
      pub fn catch<E2: $($tb)*, F>(self, f: F) -> $Eff<A, E2, R>
      where
        F: FnOnce(E) -> $Eff<A, E2, R> $($send)* + 'static,
      {
        match self.step {
          $Step::Ready(Ok(a)) => $Eff { step: $Step::Ready(Ok(a)) },
          $Step::Ready(Err(c)) => match c.split() {
            Err(e) => f(e),
            Ok(c) => $Eff { step: $Step::Ready(Err(c)) },
          },
          step => $Eff::structured(move |r| {
            $boxf(async move {
              match ($Eff { step }).run_in(&mut *r).await {
                Ok(a) => Ok(a),
                Err(c) => match c.split() {
                  Err(e) => f(e).run_in(r).await,
                  Ok(c) => Err(c),
                },
              }
            })
          }),
        }
      }

      /// Eliminate the environment at the edge (design §2.3, "Eliminate env").
      pub fn provide(self, env: R) -> $Eff<A, E, ()> {
        match self.step {
          $Step::Ready(v) => $Eff { step: $Step::Ready(v) },
          $Step::Sync(f) => $Eff::sync(move |_| {
            let mut env = env;
            f(&mut env)
          }),
          step => $Eff::structured(move |_| {
            $boxf(async move {
              let mut env = env;
              $Eff { step }.run_in(&mut env).await
            })
          }),
        }
      }

      /// Re-shape the environment from a wider one (`zoom_env` in 0.6).
      pub fn provide_env<S: $($tb)*, F>(self, f: F) -> $Eff<A, E, S>
      where
        F: FnOnce(&mut S) -> R $($send)* + 'static,
      {
        match self.step {
          $Step::Ready(v) => $Eff { step: $Step::Ready(v) },
          $Step::Sync(g) => $Eff::sync(move |s| {
            let mut env = f(s);
            g(&mut env)
          }),
          step => $Eff::structured(move |s| {
            let mut env = f(s);
            $boxf(async move { $Eff { step }.run_in(&mut env).await })
          }),
        }
      }

      /// Override an existing service for this effect only (generic over `R`; §2.3). The slot is
      /// swapped in place and restored afterwards, so `R` is not cloned.
      pub fn provide_service<T: $($tb)*>(self, service: T) -> Self
      where
        R: $crate::Replace<T>,
      {
        match self.step {
          $Step::Ready(v) => $Eff { step: $Step::Ready(v) },
          step => $Eff::structured(move |r| {
            $boxf(async move {
              let old = $crate::Replace::<T>::replace(&mut *r, service);
              let out = $Eff { step }.run_in(&mut *r).await;
              $crate::Replace::<T>::replace(r, old);
              out
            })
          }),
        }
      }
    }

    impl<A: $($tb)*, E: $($tb)*> $Eff<A, E, ()> {
      /// Lift an env-free effect into any environment.
      pub fn with_env<R2: $($tb)*>(self) -> $Eff<A, E, R2> {
        match self.step {
          $Step::Ready(v) => $Eff { step: $Step::Ready(v) },
          $Step::Sync(f) => $Eff::sync(move |_| f(&mut ())),
          $Step::Async(f) => $Eff::structured(move |_| f($crate::kernel::unit_env())),
          $Step::Leaf(f) => $Eff::from_fn_async(move |_| f($crate::kernel::unit_env())),
        }
      }
    }

    impl<A: $($tb)*, R: $($tb)*> $Eff<A, Never, R> {
      /// Retype an infallible effect's error channel (`Never` has no `Into<E>` for foreign `E`).
      pub fn any_err<E2: $($tb)*>(self) -> $Eff<A, E2, R> {
        self.map_error(|n| match n {})
      }
    }
  };
}

/// The `Send` kernel (D4: Send by default).
pub mod send {
  use super::*;

  kernel_impl! {
    /// `Effect<A, E, R>`: a lazy, `Send` description of a computation that needs `R`, may fail
    /// with `E`, and produces `A`.
    effect = Effect, step = Step, run = RunFut, fut = BoxFuture, boxf = super::box_future,
    send = [+ Send], bound = [Send + 'static]
  }
}

/// The `!Send`-capable twin (§1.2). Runs only on the local executor (`block_on_local`).
pub mod local {
  use super::*;

  kernel_impl! {
    /// `LocalEffect<A, E, R>`: like `Effect` but its steps need not be `Send`.
    effect = LocalEffect, step = LocalStep, run = LocalRunFut, fut = LocalBoxFuture,
    boxf = super::box_local_future, send = [], bound = ['static]
  }

  impl<A: 'static, E: 'static, R: 'static> From<super::send::Effect<A, E, R>>
    for LocalEffect<A, E, R>
  {
    /// `Effect -> LocalEffect` (dropping `Send`). The reverse does not exist.
    fn from(eff: super::send::Effect<A, E, R>) -> Self {
      use super::send::Step;
      match eff.step {
        Step::Ready(v) => LocalEffect {
          step: LocalStep::Ready(v),
        },
        Step::Sync(f) => LocalEffect {
          step: LocalStep::Sync(f),
        },
        Step::Async(f) => LocalEffect::structured(move |r| f(r)),
        Step::Leaf(f) => LocalEffect::from_fn_async(move |r| f(r)),
      }
    }
  }
}

pub use local::{LocalEffect, LocalRunFut};
pub use send::{Effect, RunFut};

// ── free constructors ───────────────────────────────────────────────────────────────────────

/// `Effect::succeed`, generic over `E` and `R`.
#[inline]
pub fn succeed<A: Send + 'static, E: Send + 'static, R: Send + 'static>(a: A) -> Effect<A, E, R> {
  Effect::succeed(a)
}

/// `Effect::fail`, generic over `A` and `R`.
#[inline]
pub fn fail<A: Send + 'static, E: Send + 'static, R: Send + 'static>(e: E) -> Effect<A, E, R> {
  Effect::fail(e)
}

/// An env-free future as an interruptible leaf.
pub fn from_async<A, E, R, F>(fut: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
  F: Future<Output = Result<A, E>> + Send + 'static,
{
  Effect::from_async(fut)
}

/// Yield to the executor once (an interrupt point).
pub fn yield_now<E: Send + 'static, R: Send + 'static>() -> Effect<(), E, R> {
  Effect::from_fn_async(|_r| {
    box_future(async {
      let mut yielded = false;
      std::future::poll_fn(move |cx| {
        if yielded {
          Poll::Ready(())
        } else {
          yielded = true;
          cx.waker().wake_by_ref();
          Poll::Pending
        }
      })
      .await;
      Ok(())
    })
  })
}
