//! Requesting a capability: `need::<T>()` and the `Need` builder. Filled by `k-typed-r`.
//!
//! `need::<T>()` reads capability `T` from the effect's environment `R`. Presence is checked at
//! compile time (`R: Selector<T, I>`), so it never fails and never panics (D5). The index `I` is
//! a parameter of the builder's methods, not of `need`, so `need::<T>()` needs no other turbofish.
//!
//! ```
//! use id_effect::{Effect, HCons, HNil, need};
//!
//! struct Greeting(&'static str);
//!
//! fn hello() -> Effect<String, (), HCons<Greeting, HNil>> {
//!   need::<Greeting>().map(|g| format!("{} world", g.0))
//! }
//! # let _ = hello();
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use crate::capability::Selector;
use crate::kernel::{Effect, Node};

/// Reads capability `T` from the environment (combinator style; `effect!` spells it `~T`).
/// See [`Need`].
pub fn need<T>() -> Need<T> {
  Need(PhantomData)
}

/// The builder returned by [`need`]: turn it into an effect with [`effect`](Need::effect),
/// [`map`](Need::map) or [`flat_map`](Need::flat_map). Each costs one `Arc` clone and no runloop
/// step (the read is a fused synchronous node).
pub struct Need<T>(PhantomData<fn() -> T>);

impl<T> std::fmt::Debug for Need<T> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "Need<{}>", std::any::type_name::<T>())
  }
}

impl<T: Send + Sync + 'static> Need<T> {
  /// The capability itself.
  pub fn effect<E, R, I>(self) -> Effect<Arc<T>, E, R>
  where
    R: Selector<T, I>,
  {
    Effect {
      node: Node::Sync(
        Box::new(|r: &R| Effect::pure(Ok(Arc::clone(Selector::<T, I>::get(r))))),
        1,
      ),
    }
  }

  /// A value computed from the capability.
  pub fn map<B, E, R, I>(self, f: impl FnOnce(Arc<T>) -> B + Send + 'static) -> Effect<B, E, R>
  where
    R: Selector<T, I>,
  {
    Effect {
      node: Node::Sync(
        Box::new(move |r: &R| Effect::pure(Ok(f(Arc::clone(Selector::<T, I>::get(r)))))),
        1,
      ),
    }
  }

  /// An effect computed from the capability.
  pub fn flat_map<B, E, R, I>(
    self,
    f: impl FnOnce(Arc<T>) -> Effect<B, E, R> + Send + 'static,
  ) -> Effect<B, E, R>
  where
    R: Selector<T, I>,
  {
    Effect {
      node: Node::Sync(
        Box::new(move |r: &R| f(Arc::clone(Selector::<T, I>::get(r)))),
        1,
      ),
    }
  }
}
