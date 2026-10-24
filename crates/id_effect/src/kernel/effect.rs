//! `Effect<A, E = Never, R = HNil>`, its `Node` representation (`Pure`/`Sync`/`Op`), the core
//! combinators (`map`/`flat_map`/`map_err`/`catch`/`exit`/`then`/`zip`/`widen`/`any_env`) and
//! `ExitKind`.
//!
//! Design A §1.1: an inline `Pure` node (no allocation for ready values), lazy synchronous
//! closures (`Sync`) and erased runloop programs (`Op`). `ExitKind` is defined in
//! `kernel/cause.rs`, next to the cause helpers it classifies.
//!
//! Up to `FUSE_MAX` `map`/`flat_map` steps on a ready value fuse into one typed closure; after
//! that a chain appends continuation frames to an `Instr::Seq` (no box per step for a
//! capture-free closure). The cap is 4, not 64: with 64 live boxes of one
//! size class the allocator leaves its thread cache, which cost an async bind chain more than the
//! fusion saved. The fiber-free prefix of a program still runs without a fiber or executor
//! (`settle_instr`, used by `block_on` and `~` binds).

use super::anybox::{AnyBox, AnyEnv, downcast_env, erase, hnil_env, unbox, unerase_exit};
use super::cause::{map_fail_once, split_failure, to_exit};
use super::runloop::{Frame, Instr, SeqBody, frame_vec};
use crate::capability::HNil;
use crate::failure::Never;
use crate::failure::{Cause, Exit};

/// How many `map`/`flat_map` steps fuse into one typed synchronous closure before the chain
/// spills into `Seq` frames. Short ready chains stay typed and allocation-cheap; a long chain must
/// not keep one live box per step (beyond the allocator's per-size thread cache of 7 entries the
/// boxes go through its slow path), and fused steps nest native calls.
pub(crate) const FUSE_MAX: usize = 4;

type SyncFn<A, E, R> = Box<dyn FnOnce(&R) -> Effect<A, E, R> + Send>;

/// A lazy effect: describes a computation that yields `A`, fails with `E`, and needs the
/// capabilities in `R` (an HList). It is `Send` whenever `A` and `E` are; only an
/// `Effect<_, _, HNil>` can be run.
#[must_use = "effects are lazy; run them with a run edge (`run_on`, `Runtime::run`, `block_on`)"]
pub struct Effect<A, E = Never, R = HNil> {
  pub(crate) node: Node<A, E, R>,
}

/// The representation of an [`Effect`].
pub(crate) enum Node<A, E, R> {
  /// Ready outcome (the inline `Ready` variant: no allocation).
  Pure(Result<A, Cause<E>>),
  /// Fused synchronous step: computes the next effect from the env. The `usize` is the fusion
  /// depth.
  Sync(SyncFn<A, E, R>, usize),
  /// Erased runloop program.
  Op(Instr),
}

impl<A, E, R> std::fmt::Debug for Effect<A, E, R> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self.node {
      Node::Pure(_) => "Effect::Pure",
      Node::Sync(..) => "Effect::Sync",
      Node::Op(_) => "Effect::Op",
    })
  }
}

impl<A, E, R> Effect<A, E, R> {
  /// An effect from a ready step outcome.
  #[inline]
  pub(crate) const fn pure(r: Result<A, Cause<E>>) -> Self {
    Self {
      node: Node::Pure(r),
    }
  }
}

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// An effect from an erased runloop program.
  #[inline]
  pub(crate) fn from_instr(i: Instr) -> Self {
    Self { node: Node::Op(i) }
  }

  fn sync_node(f: impl FnOnce(&R) -> Effect<A, E, R> + Send + 'static, depth: usize) -> Self {
    Self {
      node: Node::Sync(Box::new(f), depth),
    }
  }

  /// Erases this effect into a runloop program.
  ///
  /// Always inlined: it sits between a constructor and the runloop on every step, and as an
  /// out-of-line call the enum it moves goes through memory with mismatched store and load
  /// widths, which stalls store-to-load forwarding (measured: about 20 % of an async bind).
  #[inline(always)]
  pub(crate) fn into_instr(self) -> Instr {
    match self.node {
      Node::Pure(r) => Instr::Pure(super::anybox::erase_exit(r)),
      Node::Sync(f, _) => Instr::Sync(Box::new(move |env| f(downcast_env::<R>(env)).into_instr())),
      Node::Op(i) => i,
    }
  }

  /// Transforms the success value.
  pub fn map<B: Send + 'static>(self, f: impl FnOnce(A) -> B + Send + 'static) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(r) => Effect::sync_node(move |_| Effect::pure(r.map(f)), 1),
      Node::Sync(g, d) if d < FUSE_MAX => Effect::sync_node(move |r| g(r).map_now(f), d + 1),
      node => Effect::from_instr(
        Self { node }.seq(Box::new(move |v| Instr::Pure(Ok(erase(f(unbox::<A>(v))))))),
      ),
    }
  }

  /// `map` inside an already running fused step: a ready value is mapped in place.
  fn map_now<B: Send + 'static>(self, f: impl FnOnce(A) -> B + Send + 'static) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(r) => Effect::pure(r.map(f)),
      node => Self { node }.map(f),
    }
  }

  /// Sequences `f` after this effect's success.
  pub fn flat_map<B: Send + 'static>(
    self,
    f: impl FnOnce(A) -> Effect<B, E, R> + Send + 'static,
  ) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(Ok(a)) => Effect::sync_node(move |_| f(a), 1),
      Node::Pure(Err(c)) => Effect::pure(Err(c)),
      Node::Sync(g, d) if d < FUSE_MAX => Effect::sync_node(move |r| g(r).flat_map_now(f), d + 1),
      node => {
        Effect::from_instr(Self { node }.seq(Box::new(move |v| f(unbox::<A>(v)).into_instr())))
      }
    }
  }

  /// `flat_map` inside an already running fused step: a ready value is fed to `f` directly.
  fn flat_map_now<B: Send + 'static>(
    self,
    f: impl FnOnce(A) -> Effect<B, E, R> + Send + 'static,
  ) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(Ok(a)) => f(a),
      Node::Pure(Err(c)) => Effect::pure(Err(c)),
      node => Self { node }.flat_map(f),
    }
  }

  /// Appends an erased continuation: pushes onto an existing `Seq` (no box per step).
  #[inline(always)]
  fn seq(self, k: Box<dyn FnOnce(AnyBox) -> Instr + Send>) -> Instr {
    match self.node {
      Node::Op(Instr::Seq(mut body)) => {
        body.frames.push(Frame::FlatMap(k));
        Instr::Seq(body)
      }
      node => {
        let mut frames = frame_vec();
        frames.push(Frame::FlatMap(k));
        Instr::Seq(Box::new(SeqBody {
          inner: Self { node }.into_instr(),
          frames,
        }))
      }
    }
  }

  /// Sequences `next` after this effect, discarding this effect's value.
  pub fn then<B: Send + 'static>(self, next: Effect<B, E, R>) -> Effect<B, E, R> {
    self.flat_map(move |_| next)
  }

  /// Runs both effects in order and pairs their values.
  pub fn zip<B: Send + 'static>(self, other: Effect<B, E, R>) -> Effect<(A, B), E, R> {
    self.flat_map(move |a| other.map(move |b| (a, b)))
  }

  /// Transforms the typed error (defects and interrupts pass through).
  ///
  /// `f` is `FnOnce`: in a composite cause with several typed failures only the first is mapped,
  /// and each further one becomes a `Die` saying so.
  pub fn map_err<E2: Send + 'static>(
    self,
    f: impl FnOnce(E) -> E2 + Send + 'static,
  ) -> Effect<A, E2, R> {
    match self.node {
      Node::Pure(r) => {
        Effect::sync_node(move |_| Effect::pure(r.map_err(|c| map_fail_once(c, f))), 1)
      }
      node => Effect::from_instr(Instr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(move |exit| match exit {
          Ok(v) => Instr::Pure(Ok(v)),
          Err(c) => Instr::Pure(Err(map_fail_once(c, |e| erase(f(unbox::<E>(e)))))),
        }),
      )),
    }
  }

  /// Recovers from a typed failure with `f`. Defects and interrupts pass through. In a
  /// composite cause, the first typed failure (depth-first) is recovered from.
  pub fn catch<E2: Send + 'static>(
    self,
    f: impl FnOnce(E) -> Effect<A, E2, R> + Send + 'static,
  ) -> Effect<A, E2, R> {
    match self.node {
      Node::Pure(Ok(a)) => Effect::pure(Ok(a)),
      Node::Pure(Err(c)) => match split_failure::<E, E2>(c) {
        Ok(e) => Effect::sync_node(move |_| f(e), 1),
        Err(c) => Effect::pure(Err(c)),
      },
      node => Effect::from_instr(Instr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(move |exit| match exit {
          Ok(v) => Instr::Pure(Ok(v)),
          Err(c) => match split_failure::<AnyBox, AnyBox>(c) {
            Ok(e) => f(unbox::<E>(e)).into_instr(),
            Err(c) => Instr::Pure(Err(c)),
          },
        }),
      )),
    }
  }

  /// Observes the full outcome as a value (Effect.ts `Effect.exit`): never fails with `E2`.
  pub fn exit<E2: Send + 'static>(self) -> Effect<Exit<A, E>, E2, R> {
    match self.node {
      Node::Pure(r) => Effect::pure(Ok(to_exit(r))),
      node => Effect::from_instr(Instr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(|exit| Instr::Pure(Ok(erase(to_exit(unerase_exit::<A, E>(exit)))))),
      )),
    }
  }
}

impl<A: Send + 'static, R: Send + Sync + 'static> Effect<A, Never, R> {
  /// An infallible effect under any error type.
  pub fn widen<E: Send + 'static>(self) -> Effect<A, E, R> {
    self.map_err(|n| match n {})
  }
}

impl<A: Send + 'static, E: Send + 'static> Effect<A, E, HNil> {
  /// Runs this environment-free effect under any environment `R`: the result needs `R` in the
  /// type only and never reads it.
  ///
  /// Use it to hand an env-free effect to code that expects an `Effect<A, E, R>` for an `R` that
  /// need not be an HList (a procedure over a bare [`DynEnv`](crate::DynEnv), a generic `R`),
  /// where [`narrow`](Self::narrow) does not apply and a `from_ctx` body that only binds the
  /// effect would be boilerplate.
  ///
  /// Cost: a ready effect is returned as is (no allocation); a fused synchronous step is rewrapped
  /// once (one closure box, as [`narrow`](Self::narrow) does); an erased program gets one
  /// `Provide` frame whose inner environment is the shared static `HNil` (no allocation per run).
  /// The inner program runs on the same fiber, so interrupts, masks and finalizers behave as they
  /// would without the lift.
  ///
  /// ```
  /// use id_effect::{DynEnv, Effect, Exit, HNil, succeed};
  ///
  /// fn audit_count() -> Effect<u32, String> {
  ///   succeed::<u32, String, HNil>(20).map(|n| n + 1)
  /// }
  ///
  /// // A procedure over a `DynEnv`: before `any_env` this was
  /// // `Effect::from_ctx(|ctx| async move { ctx.bind(audit_count()).await })`.
  /// fn procedure() -> Effect<u32, String, DynEnv> {
  ///   audit_count().any_env()
  /// }
  ///
  /// let exit = procedure().provide_all(DynEnv::new()).block_on();
  /// assert!(matches!(exit, Exit::Success(21)));
  /// ```
  pub fn any_env<R: Send + Sync + 'static>(self) -> Effect<A, E, R> {
    match self.node {
      Node::Pure(r) => Effect::pure(r),
      Node::Sync(f, d) => Effect {
        node: Node::Sync(Box::new(move |_: &R| f(&HNil).any_env::<R>()), d),
      },
      Node::Op(i) => Effect::from_instr(Instr::Provide(
        Box::new(i),
        Box::new(|_: &AnyEnv| hnil_env().clone()),
      )),
    }
  }
}
