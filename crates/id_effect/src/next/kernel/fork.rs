//! `Effect::fork` and `Effect::fork_daemon`. Filled by `k-fiber-tree`.
//!
//! Both are built on `Instr::WithFiber`: the child is spawned from the forking fiber's context
//! and env when the fork step runs, never when the effect is built.

use std::sync::Arc;

use super::anybox::erase;
use super::effect::Effect;
use super::runloop::Instr;
use crate::next::runtime::{FiberContext, FiberHandle, spawn_fiber};
use crate::runtime::Never;

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Forks this effect as a child fiber of the current one, under the current env.
  ///
  /// The child is interrupted and awaited when its parent exits (success, failure or
  /// interrupt). Use [`widen`](Effect::widen) to fit the fork step into a fallible chain.
  pub fn fork(self) -> Effect<FiberHandle<A, E>, Never, R> {
    Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
      let child = FiberContext::new_child(fiber);
      let cell = spawn_fiber(&child, self.into_instr(), Arc::clone(env));
      Instr::Pure(Ok(erase(FiberHandle::<A, E>::new(child, cell))))
    })))
  }

  /// Forks this effect as a daemon: a root fiber with no parent, which keeps running after the
  /// forking fiber has exited.
  pub fn fork_daemon(self) -> Effect<FiberHandle<A, E>, Never, R> {
    Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
      let child = FiberContext::new_root(Arc::clone(fiber.executor()));
      let cell = spawn_fiber(&child, self.into_instr(), Arc::clone(env));
      Instr::Pure(Ok(erase(FiberHandle::<A, E>::new(child, cell))))
    })))
  }
}
