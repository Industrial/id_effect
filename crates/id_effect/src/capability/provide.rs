//! Eliminating requirements from `R`: `provide`, `provide_some`, `provide_all`, `provide_env`
//! and `narrow`. Filled by `k-typed-r`.
//!
//! Elimination happens at the type level: `provide::<T>` removes exactly one element from `R`
//! (`Plucker`), `provide_some` a whole sub-list (`Sculptor`). At run time each compiles to one
//! `Provide` frame that builds the inner environment from the outer one and restores the outer
//! environment when the inner effect ends. A ready (`Pure`) effect is returned as is: there is
//! nothing to run under the new environment.
//!
//! ```
//! use id_effect::{Effect, HCons, HNil, need};
//!
//! struct Db(u32);
//! struct Logger;
//!
//! fn query() -> Effect<u32, (), HCons<Db, HCons<Logger, HNil>>> {
//!   need::<Db>().map(|db| db.0)
//! }
//!
//! // `Db` is gone from the environment; `Logger` is still required.
//! let partly: Effect<u32, (), HCons<Logger, HNil>> = query().provide(Db(7));
//! let runnable: Effect<u32, ()> = partly.provide(Logger);
//! # let _ = runnable;
//! ```

use std::any::Any;
use std::sync::Arc;

use crate::capability::{Embed, HList, HNil, Plucker, Sculptor};
use crate::kernel::{AnyEnv, Effect, Instr, Node, die, downcast_env};

/// Moves the value out of `slot` as a `Y` when `X` and `Y` are the same type (no allocation).
fn take_if_same<X: 'static, Y: 'static>(slot: &mut Option<X>) -> Option<Y> {
  (slot as &mut dyn Any)
    .downcast_mut::<Option<Y>>()
    .and_then(Option::take)
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Provides capability `T`, eliminating it from `R` (its index `I` is inferred).
  ///
  /// Providing a capability the effect does not need is a compile error ("`T` is not required
  /// by this effect, so it cannot be provided").
  pub fn provide<T, I>(self, t: T) -> Effect<A, E, <R as Plucker<T, I>>::Rest>
  where
    T: Send + Sync + 'static,
    R: Plucker<T, I>,
  {
    match self.node {
      Node::Pure(r) => Effect::pure(r),
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |outer: &AnyEnv| {
          let rest = downcast_env::<R::Rest>(outer).clone();
          Arc::new(R::plant(Arc::new(t), rest)) as AnyEnv
        }),
      )),
    }
  }

  /// Provides a whole sub-environment `Sub` at once (any subset of `R`, in any order),
  /// eliminating its capabilities from `R`.
  pub fn provide_some<Sub, Is>(self, sub: Sub) -> Effect<A, E, <R as Sculptor<Sub, Is>>::Rest>
  where
    Sub: HList,
    R: Sculptor<Sub, Is>,
  {
    match self.node {
      Node::Pure(r) => Effect::pure(r),
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |outer: &AnyEnv| {
          let rest = downcast_env::<<R as Sculptor<Sub, Is>>::Rest>(outer).clone();
          Arc::new(R::plant(sub, rest)) as AnyEnv
        }),
      )),
    }
  }

  /// Provides exactly `R`: the result needs nothing and can be run.
  pub fn provide_all(self, env: R) -> Effect<A, E, HNil> {
    match self.node {
      Node::Pure(r) => Effect::pure(r),
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |_: &AnyEnv| Arc::new(env) as AnyEnv),
      )),
    }
  }

  /// Runs against any environment value that contains every capability of `R` (the edge: a flat
  /// application environment built once). Costs one `Arc` clone per capability of `R`.
  pub fn provide_env<Env, Is>(self, env: Env) -> Effect<A, E, HNil>
  where
    Env: Embed<R, Is> + Send + 'static,
    R: HList,
  {
    match self.node {
      Node::Pure(r) => Effect::pure(r),
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |_: &AnyEnv| Arc::new(env.project()) as AnyEnv),
      )),
    }
  }

  /// Lifts this effect into a wider environment `Wide` that contains every capability of `R`
  /// (what a `~` bind does implicitly).
  ///
  /// Narrowing to the same environment (`Wide == R`) is free: the effect is returned unchanged.
  /// Otherwise the sub-environment is projected (one `Arc` clone per capability of `R`) when the
  /// effect runs.
  pub fn narrow<Wide, Is>(self) -> Effect<A, E, Wide>
  where
    Wide: Embed<R, Is> + HList,
    R: HList,
  {
    let mut slot = Some(self);
    if let Some(same) = take_if_same::<Self, Effect<A, E, Wide>>(&mut slot) {
      return same;
    }
    let Some(this) = slot else {
      return die("id_effect: narrow lost its effect (kernel bug)");
    };
    match this.node {
      Node::Pure(r) => Effect::pure(r),
      Node::Sync(f, d) => Effect {
        node: Node::Sync(
          Box::new(move |w: &Wide| f(&w.project()).narrow::<Wide, Is>()),
          d,
        ),
      },
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(|outer: &AnyEnv| Arc::new(downcast_env::<Wide>(outer).project()) as AnyEnv),
      )),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn take_if_same_moves_only_on_identical_types() {
    let mut slot = Some(3_u8);
    assert_eq!(take_if_same::<u8, u16>(&mut slot), None);
    assert_eq!(slot, Some(3));
    assert_eq!(take_if_same::<u8, u8>(&mut slot), Some(3));
    assert_eq!(slot, None);
  }
}
