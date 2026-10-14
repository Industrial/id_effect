//! `LocalEffect` (D4 / R1): the opt-in `!Send` effect.
//!
//! Spike scope (honest): a thin async wrapper (`Ready` inline, else one boxed `!Send` future),
//! not a second instance of the runloop generated from a shared template as the design asks.
//! It runs on the calling thread (`block_on`), is not interruptible and cannot fork.
//! `Effect -> LocalEffect` is free (`.local()`); the reverse is not offered.

use std::future::Future;
use std::sync::Arc;

use crate::cause::{Cause, Exit, Never};
use crate::effect::{Ctx, Effect, Node, unerase_exit};
use crate::executor::Runtime;
use crate::fiber::{FiberContext, run_root};
use crate::hlist::{Embed, HList, HNil};
use crate::runloop::LocalBoxFuture;

type LocalFn<A, E, R> = Box<dyn FnOnce(Ctx<R>) -> LocalBoxFuture<'static, Exit<A, E>>>;

#[must_use = "effects are lazy"]
pub struct LocalEffect<A, E = Never, R = HNil> {
  node: LocalNode<A, E, R>,
}

enum LocalNode<A, E, R> {
  Ready(Exit<A, E>),
  Fut(LocalFn<A, E, R>),
}

impl<A: 'static, E: 'static, R: HList> LocalEffect<A, E, R> {
  /// The `effect_local!` target.
  pub fn from_ctx<F, Fut>(f: F) -> Self
  where
    F: FnOnce(Ctx<R>) -> Fut + 'static,
    Fut: Future<Output = Result<A, E>> + 'static,
  {
    Self {
      node: LocalNode::Fut(Box::new(move |ctx| {
        Box::pin(async move { f(ctx).await.map_err(Cause::Fail) })
      })),
    }
  }

  fn run(self, ctx: Ctx<R>) -> LocalBoxFuture<'static, Exit<A, E>> {
    match self.node {
      LocalNode::Ready(r) => Box::pin(std::future::ready(r)),
      LocalNode::Fut(f) => f(ctx),
    }
  }

  pub fn map<B: 'static>(self, f: impl FnOnce(A) -> B + 'static) -> LocalEffect<B, E, R> {
    match self.node {
      LocalNode::Ready(r) => LocalEffect {
        node: LocalNode::Ready(r.map(f)),
      },
      LocalNode::Fut(g) => LocalEffect {
        node: LocalNode::Fut(Box::new(move |ctx| {
          Box::pin(async move { g(ctx).await.map(f) })
        })),
      },
    }
  }

  pub fn flat_map<B: 'static>(
    self,
    f: impl FnOnce(A) -> LocalEffect<B, E, R> + 'static,
  ) -> LocalEffect<B, E, R> {
    LocalEffect {
      node: LocalNode::Fut(Box::new(move |ctx: Ctx<R>| {
        Box::pin(async move {
          match self.run(ctx.clone()).await {
            Ok(a) => f(a).run(ctx).await,
            Err(c) => Err(c),
          }
        })
      })),
    }
  }
}

impl<A: Send + 'static, E: Send + 'static> LocalEffect<A, E, HNil> {
  pub(crate) fn from_effect(eff: Effect<A, E, HNil>) -> Self {
    match eff.node {
      Node::Pure(r) => Self {
        node: LocalNode::Ready(r),
      },
      node => {
        let instr = Effect { node }.into_instr();
        Self {
          node: LocalNode::Fut(Box::new(move |ctx: Ctx<HNil>| {
            let exec = Arc::clone(ctx.fiber().executor());
            Box::pin(async move { unerase_exit(run_root(instr, Arc::new(HNil), exec).await) })
          })),
        }
      }
    }
  }
}

impl<A: 'static, E: 'static> LocalEffect<A, E, HNil> {
  /// Run on the calling thread, parking between wake-ups.
  pub fn block_on(self) -> Exit<A, E> {
    let rt = Runtime::global();
    let ctx = Ctx::new_root(
      Arc::new(HNil),
      FiberContext::new(None, Arc::clone(rt.executor())),
    );
    match rt.block_on_future(self.run(ctx)) {
      Ok(exit) => exit,
      Err(e) => Err(Cause::Die(crate::cause::Defect::new(e.to_string()))),
    }
  }
}

/// What `~e` binds inside `effect_local!`: a `LocalEffect` or a (`Send`) `Effect`.
pub trait IntoLocal<B, E, Sub> {
  fn into_local(self) -> LocalEffect<B, E, Sub>;
}

impl<B: 'static, E: 'static, Sub: HList> IntoLocal<B, E, Sub> for LocalEffect<B, E, Sub> {
  fn into_local(self) -> Self {
    self
  }
}

impl<B: Send + 'static, E: Send + 'static, Sub: HList> IntoLocal<B, E, Sub> for Effect<B, E, Sub> {
  fn into_local(self) -> LocalEffect<B, E, Sub> {
    LocalEffect {
      node: LocalNode::Fut(Box::new(move |ctx: Ctx<Sub>| {
        let exec = Arc::clone(ctx.fiber().executor());
        let env = ctx.env_arc();
        let instr = self.into_instr();
        Box::pin(async move { unerase_exit(run_root(instr, env, exec).await) })
      })),
    }
  }
}

impl<R: HList> Ctx<R> {
  /// `~e` inside `effect_local!`.
  pub async fn bind_local<B, E2, Sub, Is, X>(&self, eff: X) -> Result<B, E2>
  where
    X: IntoLocal<B, E2, Sub>,
    B: 'static,
    E2: 'static,
    Sub: HList,
    R: Embed<Sub, Is>,
  {
    let ctx = Ctx::new_root(Arc::new(self.env().project()), Arc::clone(self.fiber()));
    match eff.into_local().run(ctx).await {
      Ok(b) => Ok(b),
      Err(Cause::Fail(e)) => Err(e),
      Err(Cause::Die(d)) => std::panic::resume_unwind(Box::new(d.message().to_owned())),
      Err(Cause::Interrupt(id)) => {
        std::panic::resume_unwind(Box::new(crate::cause::InterruptSignal(id)))
      }
    }
  }
}
