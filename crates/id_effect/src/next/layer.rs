//! The `Layer<Out, E, In>` base: `succeed`/`from_effect`/`build`/`merge`/`provide_merge`/
//! `provide_layer`, with duplicate handling. Filled by `k-layer-base`, extended by
//! `p-layer-core`.
//!
//! A [`Layer<Out, E, In>`](Layer) is a lazy recipe that builds the environment value `Out` (an
//! HList of `Arc`ed capabilities) from the environment `In`, failing with `E` (design A §2.3).
//! The base is not memoised and not scoped (no finalizers, no `fresh`); those belong to
//! `p-layer-core` / `p-layer-scoped`.
//!
//! ```
//! use id_effect::next::{Effect, HCons, HNil, Layer, hlist, need};
//!
//! struct Config(&'static str);
//! struct Db(&'static str);
//! struct Mailer(&'static str);
//!
//! fn db() -> Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> {
//!   Layer::from_effect(need::<Config>().map(|c| hlist![Db(c.0)]))
//! }
//! fn mailer() -> Layer<HCons<Mailer, HNil>, (), HCons<Config, HNil>> {
//!   Layer::from_effect(need::<Config>().map(|c| hlist![Mailer(c.0)]))
//! }
//!
//! // Both sides need `Config`: no annotations, and both see the same `Arc<Config>`.
//! let app = db().merge(mailer()).provide_merge(Layer::succeed(hlist![Config("mem://")]));
//! let env: Effect<HCons<Config, HCons<Db, HCons<Mailer, HNil>>>, ()> = app.build();
//! # let _ = env;
//! ```
//!
//! # Shared inputs (deduplicated by construction)
//!
//! `a.merge(b)` needs `Concat<In_a, In_b>`: a capability both sides need appears twice in the
//! merged `In`. The merged input is split back **positionally** ([`SplitAt`]), never by type
//! search, so the duplicate is not ambiguous and needs no annotation (the spike's E0283). Every
//! way of supplying the merged `In` from one environment (`provide_merge`, `provide_layer`,
//! `.build().provide_env(..)`) projects it with `Embed`, whose **sub**-list may repeat a
//! capability: both positions receive the same `Arc`. Only `.provide::<T>(..)` on a merged build
//! effect stays ambiguous for a repeated `T` (`Plucker` searches by type); use `provide_env`.
//!
//! # Duplicate outputs (rejected at construction time)
//!
//! Two layers that both **produce** `T` cannot be merged: the merged `Out` would make every
//! `Selector<T, _>` lookup ambiguous. The compile-time route (a post-monomorphisation
//! `const { assert!(..) }` over the outputs' `TypeId`s) is not available on stable: probed on
//! the repo toolchain (rustc 1.99.0-nightly, MSRV 1.94), `TypeId::of` is const but `TypeId ==`
//! in a const fn is not (`PartialEq` is not yet stable as a const trait, `const_cmp`), and
//! `std::any::type_name` is not const either (`const_type_name`).
//!
//! So `merge` and `provide_merge` check the merged `Out` when they are **called** (an O(n²) scan
//! over its [`TypeIds`], always on, not a `debug_assert`) and panic with
//!
//! ```text
//! capability `<type>` is produced by both sides of Layer::merge; wrap one in a newtype
//! ```
//!
//! (`Layer::provide_merge` in the message for that combinator). Layer graphs are wired once at
//! startup, so this is a startup-time failure, never a failure inside a running fiber. This is
//! the residual limitation accepted by `k-rebuild` §7 Q-B.

use std::any::{TypeId, type_name};
use std::sync::Arc;

use crate::next::capability::{Concat, Embed, HCons, HList, HNil};
use crate::next::kernel::{AnyEnv, Effect, Instr, Node, downcast_env, succeed};

/// A lazy recipe that builds the environment value `Out` from the environment `In`, failing with
/// `E`. See the [module docs](self).
#[must_use = "layers are lazy; build them with .build() or feed them to .provide_layer(..)"]
pub struct Layer<Out, E, In = HNil> {
  build: Effect<Out, E, In>,
}

impl<Out, E, In> std::fmt::Debug for Layer<Out, E, In> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Layer")
      .field("out", &type_name::<Out>())
      .field("in", &type_name::<In>())
      .finish()
  }
}

impl<Out, E> Layer<Out, E>
where
  Out: HList,
  E: Send + 'static,
{
  /// A layer from a ready environment value. It needs nothing (`In = HNil`).
  pub fn succeed(out: Out) -> Self {
    Self {
      build: succeed(out),
    }
  }
}

impl<Out, E, In> Layer<Out, E, In>
where
  Out: HList,
  E: Send + 'static,
  In: HList,
{
  /// A layer whose construction is an effect over its input `In`.
  pub fn from_effect(build: Effect<Out, E, In>) -> Self {
    Self { build }
  }

  /// The construction effect.
  pub fn build(self) -> Effect<Out, E, In> {
    self.build
  }

  /// Side by side: `self` then `other`, outputs and inputs concatenated (`self`'s first).
  ///
  /// A capability needed by both sides appears twice in the merged input and is split back by
  /// position, so it needs no annotation; both sides receive the same `Arc` (module docs).
  ///
  /// # Panics
  ///
  /// When a capability is in both outputs (or twice in one): "capability `T` is produced by
  /// both sides of Layer::merge; wrap one in a newtype". The check runs here, when the layer
  /// graph is wired, not when it is built.
  pub fn merge<O2, I2>(
    self,
    other: Layer<O2, E, I2>,
  ) -> Layer<<Out as Concat<O2>>::Out, E, <In as Concat<I2>>::Out>
  where
    O2: HList,
    I2: HList,
    Out: Concat<O2>,
    In: Concat<I2>,
    <Out as Concat<O2>>::Out: TypeIds,
    <In as Concat<I2>>::Out: SplitAt<In, Right = I2>,
  {
    reject_duplicate_outputs::<<Out as Concat<O2>>::Out>("Layer::merge");
    let left = under(self.build, |m: &<In as Concat<I2>>::Out| {
      SplitAt::<In>::left(m)
    });
    let right = under(other.build, |m: &<In as Concat<I2>>::Out| {
      SplitAt::<In>::right(m)
    });
    Layer {
      build: left.flat_map(move |o1| right.map(move |o2| o1.concat(o2))),
    }
  }

  /// Strata: builds `base`, feeds its output into this layer's input, and keeps both outputs
  /// (`base`'s first; Effect.ts `Layer.provideMerge`). `base` must produce every capability of
  /// `In` (any order, extras allowed); the result needs only `base`'s input.
  ///
  /// # Panics
  ///
  /// When a capability is produced by both `base` and `self`: "capability `T` is produced by
  /// both sides of Layer::provide_merge; wrap one in a newtype".
  pub fn provide_merge<O0, I0, Is>(
    self,
    base: Layer<O0, E, I0>,
  ) -> Layer<<O0 as Concat<Out>>::Out, E, I0>
  where
    O0: HList + Concat<Out> + Embed<In, Is>,
    I0: HList,
    <O0 as Concat<Out>>::Out: TypeIds,
  {
    reject_duplicate_outputs::<<O0 as Concat<Out>>::Out>("Layer::provide_merge");
    let upper = self.build;
    Layer {
      build: base.build.flat_map(move |o0: O0| {
        upper
          .provide_env(o0.clone())
          .narrow::<I0, HNil>()
          .map(move |out| o0.concat(out))
      }),
    }
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: HList,
{
  /// Builds `layer` and runs this effect against its output, which must contain every
  /// capability of `R` (any order, extras allowed). The result needs only the layer's input.
  /// The layer is built each time the effect runs (the base is not memoised).
  pub fn provide_layer<Out, In, Is>(self, layer: Layer<Out, E, In>) -> Effect<A, E, In>
  where
    Out: HList + Embed<R, Is>,
    In: HList,
  {
    layer
      .build
      .flat_map(move |out| self.provide_env(out).narrow::<In, HNil>())
  }
}

/// Positional split of an environment whose first part is `Left` (what `Layer::merge` uses to
/// hand each side its own input). Walks the structure of `Self`, not types, so a capability in
/// both parts is not ambiguous.
#[diagnostic::on_unimplemented(
  message = "the environment `{Self}` does not start with `{Left}`",
  label = "cannot split `{Self}` after `{Left}`"
)]
pub trait SplitAt<Left>: HList {
  /// What follows `Left`.
  type Right: HList;
  /// The first part (one `Arc` clone per capability of `Left`).
  fn left(&self) -> Left;
  /// The rest (one `Arc` clone per capability of `Right`).
  fn right(&self) -> Self::Right;
}

impl<S: HList> SplitAt<HNil> for S {
  type Right = S;
  fn left(&self) -> HNil {
    HNil
  }
  fn right(&self) -> S {
    self.clone()
  }
}

#[diagnostic::do_not_recommend]
impl<H, T, Rest> SplitAt<HCons<H, T>> for HCons<H, Rest>
where
  H: Send + Sync + 'static,
  T: HList,
  Rest: SplitAt<T>,
{
  type Right = Rest::Right;
  fn left(&self) -> HCons<H, T> {
    HCons(Arc::clone(&self.0), self.1.left())
  }
  fn right(&self) -> Self::Right {
    self.1.right()
  }
}

/// The `TypeId`s and type names of an environment's capabilities, in order (the duplicate-output
/// check of `Layer::merge` / `Layer::provide_merge`).
pub trait TypeIds: HList {
  /// Appends `(TypeId, type name)` for every capability, head first.
  fn push_type_ids(out: &mut Vec<(TypeId, &'static str)>);

  /// Every capability's `(TypeId, type name)`, head first.
  fn type_ids() -> Vec<(TypeId, &'static str)> {
    let mut out = Vec::with_capacity(Self::LEN);
    Self::push_type_ids(&mut out);
    out
  }
}

impl TypeIds for HNil {
  fn push_type_ids(_: &mut Vec<(TypeId, &'static str)>) {}
}

impl<H: Send + Sync + 'static, T: TypeIds> TypeIds for HCons<H, T> {
  fn push_type_ids(out: &mut Vec<(TypeId, &'static str)>) {
    out.push((TypeId::of::<H>(), type_name::<H>()));
    T::push_type_ids(out);
  }
}

/// Panics when `Out` holds a capability twice (O(n²), n = `Out::LEN`).
fn reject_duplicate_outputs<Out: TypeIds>(op: &str) {
  let ids = Out::type_ids();
  for (i, (id, name)) in ids.iter().enumerate() {
    if ids[..i].iter().any(|(seen, _)| seen == id) {
      panic!("capability `{name}` is produced by both sides of {op}; wrap one in a newtype");
    }
  }
}

/// Runs `eff` (over `Inner`) inside an environment `Outer`, building its environment with
/// `project` when it runs. A ready effect is returned as is.
fn under<A, E, Inner, Outer>(
  eff: Effect<A, E, Inner>,
  project: impl FnOnce(&Outer) -> Inner + Send + 'static,
) -> Effect<A, E, Outer>
where
  A: Send + 'static,
  E: Send + 'static,
  Inner: HList,
  Outer: HList,
{
  match eff.node {
    Node::Pure(r) => Effect::pure(r),
    node => Effect::from_instr(Instr::Provide(
      Box::new(Effect::<A, E, Inner> { node }.into_instr()),
      Box::new(move |outer: &AnyEnv| Arc::new(project(downcast_env::<Outer>(outer))) as AnyEnv),
    )),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct A;
  struct B;

  #[test]
  fn split_at_is_positional_with_repeats() {
    let env = HCons::new(A, HCons::new(A, HCons::new(B, HNil)));
    let l: HCons<A, HNil> = SplitAt::<HCons<A, HNil>>::left(&env);
    let r: HCons<A, HCons<B, HNil>> = SplitAt::<HCons<A, HNil>>::right(&env);
    assert!(Arc::ptr_eq(&l.0, &env.0));
    assert!(Arc::ptr_eq(&r.0, &env.1.0));
  }

  #[test]
  fn type_ids_in_order_and_duplicates_rejected() {
    let ids = <HCons<A, HCons<B, HNil>>>::type_ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0].0, TypeId::of::<A>());
    reject_duplicate_outputs::<HCons<A, HCons<B, HNil>>>("t");
    let dup =
      std::panic::catch_unwind(|| reject_duplicate_outputs::<HCons<A, HCons<A, HNil>>>("t"));
    assert!(dup.is_err());
  }
}
