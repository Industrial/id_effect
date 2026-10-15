//! `Ctx<R>`: the environment (`Arc<R>`) and the current fiber (`Arc<FiberContext>`) seen by a
//! running effect, with `env()`/`fiber()`.
//!
//! Typed access (`Ctx::get`, `Ctx::bind`) lives in `kernel/bind.rs`; the fields
//! are `pub(crate)` for that.

use std::sync::Arc;

use crate::runtime::FiberContext;

/// What an async effect body owns: the environment (shared) and the current fiber. Cloning costs
/// two refcount bumps.
pub struct Ctx<R> {
  pub(crate) env: Arc<R>,
  pub(crate) fiber: Arc<FiberContext>,
}

impl<R> Clone for Ctx<R> {
  fn clone(&self) -> Self {
    Self {
      env: Arc::clone(&self.env),
      fiber: Arc::clone(&self.fiber),
    }
  }
}

impl<R> std::fmt::Debug for Ctx<R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Ctx")
      .field("fiber", &self.fiber.id())
      .finish_non_exhaustive()
  }
}

impl<R> Ctx<R> {
  /// A context over `env` on `fiber`.
  pub(crate) fn new(env: Arc<R>, fiber: Arc<FiberContext>) -> Self {
    Self { env, fiber }
  }

  /// The environment value.
  pub fn env(&self) -> &R {
    &self.env
  }

  /// The fiber this body runs on.
  pub fn fiber(&self) -> &Arc<FiberContext> {
    &self.fiber
  }
}
