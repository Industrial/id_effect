//! Effect constructors: `succeed`/`fail`/`die`/`sync`/`suspend`/`from_async`/`from_ctx`/`sleep`/
//! `yield_fiber`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use super::anybox::{downcast_env_arc, erase, erase_exit};
use super::ctx::Ctx;
use super::effect::{Effect, Node};
use super::runloop::Instr;
use crate::failure::{Cause, Defect};

/// A ready success (no allocation).
pub fn succeed<A, E, R>(a: A) -> Effect<A, E, R> {
  Effect::pure(Ok(a))
}

/// A ready typed failure (no allocation).
pub fn fail<A, E, R>(e: E) -> Effect<A, E, R> {
  Effect::pure(Err(Cause::Fail(e)))
}

/// A defect: the effect dies with `defect` (a [`Defect`], `String` or `&str` message).
pub fn die<A, E, R>(defect: impl Into<Defect>) -> Effect<A, E, R> {
  Effect::pure(Err(Cause::Die(defect.into())))
}

/// Defers building an effect until it runs.
pub fn suspend<A, E, R>(f: impl FnOnce() -> Effect<A, E, R> + Send + 'static) -> Effect<A, E, R> {
  Effect {
    node: Node::Sync(Box::new(move |_| f()), 1),
  }
}

/// A lazy synchronous computation. A panic in `f` becomes `Cause::Die`.
pub fn sync<A, E, R>(f: impl FnOnce() -> A + Send + 'static) -> Effect<A, E, R> {
  Effect {
    node: Node::Sync(Box::new(move |_| succeed(f())), 1),
  }
}

/// A real async step. `f` runs on first poll; the future must be `Send + 'static` (it may not
/// borrow the environment; use [`Effect::from_ctx`] for that).
pub fn from_async<A, E, R, F, Fut>(f: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
  F: FnOnce() -> Fut + Send + 'static,
  Fut: Future<Output = Result<A, E>> + Send + 'static,
{
  // One allocation: the boxed async block is lazy (`f` runs on first poll), so it needs no
  // separate builder closure.
  Effect::from_instr(Instr::Future(Box::pin(async move {
    erase_exit(f().await.map_err(Cause::Fail))
  })))
}

/// Suspends the fiber for `d` on its executor's timer (virtual time on a test executor).
pub fn sleep<E: Send + 'static, R: Send + Sync + 'static>(d: Duration) -> Effect<(), E, R> {
  Effect::from_instr(Instr::Async(Box::new(move |_, fiber| {
    let s = fiber.executor().sleep(d);
    Box::pin(async move {
      s.await;
      Ok(erase(()))
    })
  })))
}

/// Yields the fiber once to its executor: the runloop is woken immediately and re-queued.
pub fn yield_fiber<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<(), E, R> {
  Effect::from_instr(Instr::Async(Box::new(|_, _| {
    Box::pin(async {
      YieldOnce(false).await;
      Ok(erase(()))
    })
  })))
}

/// Pending once (after waking its own waker), then ready.
struct YieldOnce(bool);

impl Future for YieldOnce {
  type Output = ();
  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    if self.0 {
      Poll::Ready(())
    } else {
      self.0 = true;
      cx.waker().wake_by_ref();
      Poll::Pending
    }
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// An async body that owns a [`Ctx<R>`] (the `effect!` target): the environment and the
  /// current fiber, cloned once when the step starts.
  pub fn from_ctx<F, Fut>(f: F) -> Self
  where
    F: FnOnce(Ctx<R>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<A, E>> + Send + 'static,
  {
    Self::from_instr(Instr::Async(Box::new(move |env, fiber| {
      let ctx = Ctx::new(downcast_env_arc::<R>(env), Arc::clone(fiber));
      let fut = f(ctx);
      Box::pin(async move { erase_exit(fut.await.map_err(Cause::Fail)) })
    })))
  }
}
