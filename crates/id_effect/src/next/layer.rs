//! `Layer<Out, E, In>`: lazy, memoised recipes for environment values. `succeed` / `effect` /
//! `from_effect` / `scoped` / `acquire_release` / `fresh` / `build` / `build_scoped` / `merge` /
//! `provide_to` / `provide_merge` / `launch` and `Effect::provide_layer`. Filled by
//! `k-layer-base`, `p-layer-core` and `p-layer-scoped`.
//!
//! A [`Layer<Out, E, In>`](Layer) is a lazy recipe that builds the environment value `Out` (an
//! HList of `Arc`ed capabilities) from the environment `In`, failing with `E` (design A §2.3).
//! Building it ([`Layer::build`], [`Effect::provide_layer`]) yields an ordinary effect; once every
//! input is supplied that effect needs nothing (`Effect<_, E, HNil>`) and can be run.
//!
//! ```
//! use id_effect::next::{Effect, HCons, HNil, Layer, hlist, need};
//!
//! struct Config(&'static str);
//! struct Db(&'static str);
//! struct Mailer(&'static str);
//!
//! fn db() -> Layer<HCons<Db, HNil>, (), HCons<Config, HNil>> {
//!   Layer::effect(|| need::<Config>().map(|c| hlist![Db(c.0)]))
//! }
//! fn mailer() -> Layer<HCons<Mailer, HNil>, (), HCons<Config, HNil>> {
//!   Layer::effect(|| need::<Config>().map(|c| hlist![Mailer(c.0)]))
//! }
//!
//! // Both sides need `Config`: no annotations, and both see the same `Arc<Config>`.
//! let app = db().merge(mailer()).provide_merge(Layer::succeed(hlist![Config("mem://")]));
//! let env: Effect<HCons<Config, HCons<Db, HCons<Mailer, HNil>>>, ()> = app.build();
//! # let _ = env;
//! ```
//!
//! # Memoisation (Effect.ts `Layer` semantics)
//!
//! A layer is memoised **by reference within one build**: a `Layer` value is cheap to clone (an
//! `Arc` bump) and every clone is the same layer, so a layer used in several places of one graph
//! is constructed once and every user receives the same capability `Arc`s. Two separate calls
//! of a layer-returning function are two layers and build twice, as two `Layer.effect(..)` calls
//! do in Effect.ts.
//!
//! ```
//! use std::sync::Arc;
//! use std::sync::atomic::{AtomicUsize, Ordering};
//! use id_effect::next::{HCons, HNil, Layer, hlist, need, sync};
//!
//! struct Pool;
//! struct Users(Arc<Pool>);
//! struct Orders(Arc<Pool>);
//!
//! let opened = Arc::new(AtomicUsize::new(0));
//! let seen = Arc::clone(&opened);
//! let pool: Layer<HCons<Pool, HNil>, ()> = Layer::effect(move || {
//!   let seen = Arc::clone(&seen);
//!   sync(move || {
//!     seen.fetch_add(1, Ordering::SeqCst);
//!     hlist![Pool]
//!   })
//! });
//! fn users() -> Layer<HCons<Users, HNil>, (), HCons<Pool, HNil>> {
//!   Layer::effect(|| need::<Pool>().map(|p| hlist![Users(p)]))
//! }
//! fn orders() -> Layer<HCons<Orders, HNil>, (), HCons<Pool, HNil>> {
//!   Layer::effect(|| need::<Pool>().map(|p| hlist![Orders(p)]))
//! }
//!
//! // `pool` is used twice (a clone is the same layer): built once, one `Arc<Pool>`.
//! let app = pool.clone().provide_to(users()).merge(pool.provide_to(orders()));
//! let id_effect::Exit::Success(env) = app.build().block_on() else {
//!   panic!("the build failed")
//! };
//! assert!(Arc::ptr_eq(&env.0.0, &env.1.0.0)); // one pool ...
//! assert_eq!(opened.load(Ordering::SeqCst), 1); // ... opened once
//! ```
//!
//! The memo table lives for one build: each [`Layer::build`] (and each run of an effect from
//! [`Effect::provide_layer`]) starts empty, so separate builds construct their layers again. A
//! failed construction is not memoised. The first build of a layer within a build wins: its
//! result is shared by every later use in that build, whatever environment that use runs in.
//! Builds are sequential (`merge` builds its left side, then its right side), so an entry is
//! either absent or complete.
//!
//! [`Layer::fresh`] opts a layer out of this sharing (Effect.ts `Layer.fresh`): it builds its
//! inner graph under a new, empty memo table, so it is constructed again even where the same
//! layer is already built. Its finalizers still go to the build's scope.
//!
//! # Scoped layers (finalizers)
//!
//! Every build has one [`Scope`]. A layer made with [`Layer::scoped`] receives it and registers
//! finalizers into it; [`Layer::acquire_release`] is the common case (acquire uninterruptibly,
//! register the release). A finalizer is an effect (`Effect<(), Never, HNil>`), so it can be
//! asynchronous: this replaces v1's synchronous `ProviderSpec::on_shutdown`. A memoised layer
//! registers its finalizers once per build.
//!
//! The scope closes when its owner ends, running the finalizers once, uninterruptibly and in
//! reverse order of acquisition, with the [`ExitKind`] of the owner:
//!
//! - [`Effect::provide_layer`] owns the scope of its build: it closes after the effect completes
//!   (success, failure, defect or interrupt), as Effect.ts `Effect.provide(layer)` does;
//! - [`Layer::launch`] builds into a scope that closes only when the launched effect is
//!   interrupted (Effect.ts `Layer.launch`);
//! - [`Layer::build_scoped`] builds into a caller's [`Scope`] (Effect.ts `Layer.buildWithScope`).
//!
//! [`Layer::build`] has no owner for its scope: it closes it when the build fails or is
//! interrupted (releasing what was acquired so far), but after a successful build its
//! finalizers never run. Use one of the owners above for a layer that holds resources.
//!
//! ```
//! use std::sync::{Arc, Mutex};
//! use id_effect::next::{Effect, HCons, HNil, Layer, hlist, need, sync};
//!
//! struct Pool;
//! struct Repo;
//!
//! let log = Arc::new(Mutex::new(Vec::new()));
//! let (l1, l2, l3) = (Arc::clone(&log), Arc::clone(&log), Arc::clone(&log));
//! let pool: Layer<HCons<Pool, HNil>, ()> = Layer::acquire_release(
//!   || sync(|| hlist![Pool]),
//!   move |_, _| {
//!     let l = Arc::clone(&l1);
//!     sync(move || l.lock().unwrap().push("close pool"))
//!   },
//! );
//! let repo: Layer<HCons<Repo, HNil>, (), HCons<Pool, HNil>> = Layer::acquire_release(
//!   || need::<Pool>().map(|_| hlist![Repo]),
//!   move |_, _| {
//!     let l = Arc::clone(&l2);
//!     sync(move || l.lock().unwrap().push("close repo"))
//!   },
//! );
//!
//! let use_repo: Effect<(), (), HCons<Repo, HNil>> =
//!   need::<Repo>().flat_map(move |_| sync(move || l3.lock().unwrap().push("use")));
//! let app: Effect<(), ()> = use_repo.provide_layer(pool.provide_to(repo));
//! assert!(matches!(app.block_on(), id_effect::Exit::Success(())));
//! // Released after the effect, in reverse order of acquisition.
//! assert_eq!(*log.lock().unwrap(), ["use", "close repo", "close pool"]);
//! ```
//!
//! # Shared inputs (deduplicated by construction)
//!
//! `a.merge(b)` needs `Concat<In_a, In_b>`: a capability both sides need appears twice in the
//! merged `In`. The merged input is split back **positionally** ([`SplitAt`]), never by type
//! search, so the duplicate is not ambiguous and needs no annotation (the spike's E0283). Every
//! way of supplying the merged `In` from one environment (`provide_to`, `provide_merge`,
//! `provide_layer`, `.build().provide_env(..)`) projects it with `Embed`, whose **sub**-list may
//! repeat a capability: both positions receive the same `Arc`. Only `.provide::<T>(..)` on a
//! merged build effect stays ambiguous for a repeated `T` (`Plucker` searches by type); use
//! `provide_env`.
//!
//! # Duplicate outputs (rejected when the graph is wired)
//!
//! Two layers that both **produce** `T` cannot be merged: the merged `Out` would make every
//! `Selector<T, _>` lookup ambiguous. The compile-time route (a post-monomorphisation
//! `const { assert!(..) }` over the outputs' `TypeId`s) is not available on stable: probed on
//! the repo toolchain (rustc 1.99.0-nightly, MSRV 1.94), `TypeId::of` is const but `TypeId ==`
//! in a const fn is not (`PartialEq` is not yet stable as a const trait, `const_cmp`), and
//! `std::any::type_name` is not const either (`const_type_name`). A trait-level "not equal"
//! bound would need negative reasoning (auto traits / specialisation), which is unstable too.
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
//! the residual limitation accepted by `k-rebuild` §7 Q-B, re-examined and kept by
//! `p-layer-core`. `provide_to` keeps only the upper layer's output, so it cannot duplicate one.

use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::next::capability::{Concat, Embed, HCons, HList, HNil};
use crate::next::kernel::{
  AnyEnv, Effect, ExitKind, Instr, MaskOp, Node, die, downcast_env, from_async, succeed, suspend,
};
use crate::next::scope::Scope;
use crate::runtime::Never;

/// One node's construction, given the build's memo table.
type BuildFn<Out, E, In> = dyn Fn(&MemoMap) -> Effect<Out, E, In> + Send + Sync;

/// A lazy, memoised recipe that builds the environment value `Out` from the environment `In`,
/// failing with `E`. Cloning is an `Arc` bump and yields the same layer (memoised together).
/// See the [module docs](self).
#[must_use = "layers are lazy; build them with .build() or feed them to .provide_layer(..)"]
pub struct Layer<Out, E, In = HNil> {
  node: Arc<BuildFn<Out, E, In>>,
}

impl<Out, E, In> Clone for Layer<Out, E, In> {
  fn clone(&self) -> Self {
    Self {
      node: Arc::clone(&self.node),
    }
  }
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
  /// A layer from a ready environment value (Effect.ts `Layer.succeed`). It needs nothing
  /// (`In = HNil`); every build hands out the same `Arc`s.
  pub fn succeed(out: Out) -> Self {
    Self::from_build(move |_| succeed(out.clone()))
  }
}

impl<Out, E, In> Layer<Out, E, In>
where
  Out: HList,
  E: Send + 'static,
  In: HList,
{
  /// A layer whose construction is the effect `make()` over its input `In` (Effect.ts
  /// `Layer.effect`). `make` is called once per build that needs this layer, so the layer can be
  /// built any number of times.
  pub fn effect(make: impl Fn() -> Effect<Out, E, In> + Send + Sync + 'static) -> Self {
    Self::from_build(move |_| make())
  }

  /// A layer whose construction is the one-shot effect `build`. Effects run once, so this layer
  /// can be built once: memoisation shares that one result within a build, and a second build
  /// dies with a defect. Use [`Layer::effect`] for a layer that is built more than once.
  pub fn from_effect(build: Effect<Out, E, In>) -> Self {
    let slot = Mutex::new(Some(build));
    Self::from_build(move |_| {
      let taken = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
      taken.unwrap_or_else(|| {
        die(format!(
          "Layer::from_effect: the layer producing `{}` was already built once; use \
           Layer::effect to build a layer more than once",
          type_name::<Out>()
        ))
      })
    })
  }

  /// A layer whose construction `make(scope)` may register finalizers into the build's
  /// [`Scope`] (Effect.ts `Layer.scoped`). They run when the scope closes: after the effect a
  /// [`Effect::provide_layer`] runs, when a [`Layer::launch`] is interrupted, or when the caller
  /// of [`Layer::build_scoped`] closes its scope; in reverse order of registration, with the
  /// closing [`ExitKind`]. `make` is called once per build that needs this layer.
  pub fn scoped(make: impl Fn(Scope) -> Effect<Out, E, In> + Send + Sync + 'static) -> Self {
    Self::from_build(move |memo| make(memo.scope.clone()))
  }

  /// A resource layer (Effect.ts `Layer.scoped(Effect.acquireRelease(..))`): `acquire()` runs
  /// uninterruptibly and, when it succeeds, `release(out, kind)` is registered in the build's
  /// scope as one step, so an interrupt cannot fall between them. `release` is an effect (it may
  /// be asynchronous) and runs once, when the scope closes, fed the closing [`ExitKind`]. A
  /// failed `acquire` registers nothing.
  pub fn acquire_release(
    acquire: impl Fn() -> Effect<Out, E, In> + Send + Sync + 'static,
    release: impl Fn(Out, ExitKind) -> Effect<(), Never, HNil> + Send + Sync + 'static,
  ) -> Self {
    let release = Arc::new(release);
    Self::scoped(move |scope| {
      let release = Arc::clone(&release);
      acquire()
        .flat_map(move |out: Out| {
          let held = out.clone();
          scope
            .add_finalizer_exit(move |kind| release(held, kind))
            .map(move |()| out)
        })
        .uninterruptible()
    })
  }

  /// This layer, unshared (Effect.ts `Layer.fresh`): its graph is built under a new, empty memo
  /// table, so it is constructed again even where the same layer is already built in this
  /// build, and nothing inside it is shared with the rest of the build. Its finalizers still go
  /// to the build's scope (in acquisition order with the rest).
  pub fn fresh(self) -> Self {
    Layer::from_build(move |memo| self.build_in(&memo.fresh()))
  }

  /// A new node (its own memo identity) from its construction.
  fn from_build(build: impl Fn(&MemoMap) -> Effect<Out, E, In> + Send + Sync + 'static) -> Self {
    Self {
      node: Arc::new(build),
    }
  }

  /// The construction effect, under a new memo table and a new scope (each call builds afresh;
  /// within the build every layer is constructed at most once).
  ///
  /// Nothing owns that scope: it is closed when the build fails or is interrupted, releasing
  /// what was acquired so far, but a successful build's finalizers never run. For a layer with
  /// finalizers use [`Effect::provide_layer`], [`Layer::launch`] or [`Layer::build_scoped`].
  pub fn build(&self) -> Effect<Out, E, In> {
    let scope = Scope::new();
    let body = self.build_scoped(&scope);
    Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let m = fiber.mask();
      Instr::Mask(
        Box::new(Instr::Fold(
          Box::new(Instr::Mask(Box::new(body.into_instr()), MaskOp::Set(m))),
          Box::new(move |exit| match exit {
            Ok(out) => Instr::Pure(Ok(out)),
            Err(cause) => {
              let kind = ExitKind::of_cause(&cause);
              let close = under(scope.close(kind), |_: &In| HNil).into_instr();
              // `close` cannot fail (finalizer defects aside, which are sequenced after).
              Instr::Fold(
                Box::new(close),
                Box::new(move |fin| {
                  Instr::Pure(Err(match fin {
                    Ok(_) => cause,
                    Err(fc) => crate::failure::Cause::then(cause, fc),
                  }))
                }),
              )
            }
          }),
        )),
        MaskOp::Inc,
      )
    })))
  }

  /// The construction effect, under a new memo table, registering every finalizer into `scope`
  /// (Effect.ts `Layer.buildWithScope`). The caller closes `scope`; closing it runs the
  /// finalizers in reverse order of acquisition.
  pub fn build_scoped(&self, scope: &Scope) -> Effect<Out, E, In> {
    self.build_in(&MemoMap::with_scope(scope.clone()))
  }

  /// Builds this layer and keeps it alive until interrupted (Effect.ts `Layer.launch`): the
  /// effect never succeeds; interrupting it closes the build's scope, running the finalizers in
  /// reverse order. A failed build fails the effect (and releases what was acquired).
  pub fn launch(self) -> Effect<Never, E, In> {
    Effect::scoped(move |scope| {
      self
        .build_scoped(&scope)
        .flat_map(|_| from_async(std::future::pending::<Result<Never, E>>))
    })
  }

  /// This layer's construction within the build whose memo table is `memo`: a memoised result
  /// is reused, otherwise the node is constructed and its result recorded.
  fn build_in(&self, memo: &MemoMap) -> Effect<Out, E, In> {
    let node = Arc::clone(&self.node);
    let memo = memo.clone();
    suspend(move || {
      let key = memo_key(&node);
      if let Some(out) = memo.get::<Out>(key) {
        return succeed(out);
      }
      let construct = node(&memo);
      construct.map(move |out: Out| {
        memo.insert(key, Box::new(node), out.clone());
        out
      })
    })
  }

  /// Side by side (Effect.ts `Layer.merge`): `self` then `other`, outputs and inputs
  /// concatenated (`self`'s first). A layer used by both sides is built once.
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
    Layer::from_build(move |memo| {
      let left = under(self.build_in(memo), |m: &<In as Concat<I2>>::Out| {
        SplitAt::<In>::left(m)
      });
      let right = under(other.build_in(memo), |m: &<In as Concat<I2>>::Out| {
        SplitAt::<In>::right(m)
      });
      left.flat_map(move |o1| right.map(move |o2| o1.concat(o2)))
    })
  }

  /// Feeds this layer's output into `upper`'s input and keeps only `upper`'s output (Effect.ts
  /// `Layer.provide`, with the provider as receiver: `base.provide_to(upper)`). `self` must
  /// produce every capability of `I2` (any order, extras allowed); the result needs only this
  /// layer's input.
  pub fn provide_to<O2, I2, Is>(self, upper: Layer<O2, E, I2>) -> Layer<O2, E, In>
  where
    O2: HList,
    I2: HList,
    Out: Embed<I2, Is>,
  {
    Layer::from_build(move |memo| {
      let up = upper.build_in(memo);
      self
        .build_in(memo)
        .flat_map(move |out| up.provide_env(out).narrow::<In, HNil>())
    })
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
    Layer::from_build(move |memo| {
      let upper = self.build_in(memo);
      base.build_in(memo).flat_map(move |o0: O0| {
        upper
          .provide_env(o0.clone())
          .narrow::<I0, HNil>()
          .map(move |out| o0.concat(out))
      })
    })
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: HList,
{
  /// Builds `layer` and runs this effect against its output, which must contain every
  /// capability of `R` (any order, extras allowed); Effect.ts `Effect.provide(layer)`. The
  /// result needs only the layer's input: with a closed layer (`In = HNil`) it is runnable.
  ///
  /// Each run builds the layer under a new memo table and a new [`Scope`], which closes after
  /// this effect completes (success, failure, defect or interrupt, also when the build itself
  /// fails or is interrupted): the layers' finalizers run then, in reverse order of acquisition.
  pub fn provide_layer<Out, In, Is>(self, layer: Layer<Out, E, In>) -> Effect<A, E, In>
  where
    Out: HList + Embed<R, Is>,
    In: HList,
  {
    Effect::scoped(move |scope| {
      layer
        .build_scoped(&scope)
        .flat_map(move |out| self.provide_env(out).narrow::<In, HNil>())
    })
  }
}

/// The memo table of one build: each constructed node's output, keyed by the node's address,
/// and the build's [`Scope`], which receives every finalizer. An entry also holds the node
/// itself, so the address cannot be reused while the table lives. Clones share the table and the
/// scope.
#[derive(Clone, Default)]
struct MemoMap {
  entries: Arc<Mutex<HashMap<usize, MemoEntry>>>,
  scope: Scope,
}

struct MemoEntry {
  /// Keeps the node alive (its address is the key).
  _node: Box<dyn Any + Send + Sync>,
  /// The node's output (`Out`).
  out: Box<dyn Any + Send + Sync>,
}

impl MemoMap {
  /// An empty table whose finalizers go to `scope`.
  fn with_scope(scope: Scope) -> Self {
    Self {
      entries: Arc::default(),
      scope,
    }
  }

  /// An empty table sharing this one's scope (`Layer::fresh`).
  fn fresh(&self) -> Self {
    Self::with_scope(self.scope.clone())
  }

  /// The memoised output of the node at `key`.
  fn get<Out: HList>(&self, key: usize) -> Option<Out> {
    let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
    entries
      .get(&key)
      .and_then(|e| e.out.downcast_ref::<Out>())
      .cloned()
  }

  /// Records the output of the node at `key` (the first record wins).
  fn insert<Out: HList>(&self, key: usize, node: Box<dyn Any + Send + Sync>, out: Out) {
    let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
    entries.entry(key).or_insert(MemoEntry {
      _node: node,
      out: Box::new(out),
    });
  }
}

/// A node's memo identity: its address.
fn memo_key<Out, E, In>(node: &Arc<BuildFn<Out, E, In>>) -> usize {
  Arc::as_ptr(node).cast::<()>() as usize
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

  #[test]
  fn memo_map_keys_by_node_and_keeps_the_first_entry() {
    let layer: Layer<HCons<A, HNil>, ()> = Layer::succeed(HCons::new(A, HNil));
    let key = memo_key(&layer.node);
    assert_eq!(
      key,
      memo_key(&layer.clone().node),
      "clones share the identity"
    );
    let memo = MemoMap::default();
    assert!(memo.get::<HCons<A, HNil>>(key).is_none());
    let first = HCons::new(A, HNil);
    memo.insert(key, Box::new(Arc::clone(&layer.node)), first.clone());
    memo.insert(key, Box::new(Arc::clone(&layer.node)), HCons::new(A, HNil));
    let got = memo.get::<HCons<A, HNil>>(key).expect("memoised");
    assert!(Arc::ptr_eq(&got.0, &first.0));
  }
}
