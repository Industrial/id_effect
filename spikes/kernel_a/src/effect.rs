//! `Effect<A, E, R>` (design A §1.1): an inline `Pure` node (R1: no alloc for ready values),
//! fused synchronous closures (`Sync`, bounded depth), and erased runloop programs (`Op`).

use std::future::Future;
use std::marker::PhantomData;
use std::panic::resume_unwind;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::cause::{Cause, Defect, Exit, ExitKind, InterruptSignal, Never};
use crate::executor::Runtime;
use crate::fiber::{FiberContext, FiberHandle, spawn_fiber};
use crate::hlist::{Embed, HList, HNil, Plucker, Sculptor, Selector};
pub(crate) use crate::runloop::unbox;
use crate::runloop::{AnyEnv, AnyExit, Instr, MaskOp, Runloop, erase};

/// How many `map`/`flat_map` steps fuse into one synchronous closure before the chain spills into
/// runloop frames. Bounds native stack use (fusion nests calls).
const FUSE_MAX: u16 = 64;

type SyncFn<A, E, R> = Box<dyn FnOnce(&R) -> Effect<A, E, R> + Send>;

/// A lazy, `Send` (iff `A`, `E` are), typed-environment effect. `R` is an HList of capabilities;
/// only `Effect<_, _, HNil>` can be run.
#[must_use = "effects are lazy; run them with block_on / Runtime::run"]
pub struct Effect<A, E = Never, R = HNil> {
  pub(crate) node: Node<A, E, R>,
}

pub(crate) enum Node<A, E, R> {
  /// Ready value (R1 inline `Ready` variant).
  Pure(Exit<A, E>),
  /// Fused synchronous step: computes the next effect from the env. The `u16` is the fusion depth.
  Sync(SyncFn<A, E, R>, u16),
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

pub(crate) fn erase_exit<A: Send + 'static, E: Send + 'static>(r: Exit<A, E>) -> AnyExit {
  match r {
    Ok(a) => Ok(erase(a)),
    Err(c) => Err(c.map(erase)),
  }
}

pub(crate) fn unerase_exit<A: 'static, E: 'static>(r: AnyExit) -> Exit<A, E> {
  match r {
    Ok(a) => Ok(unbox(a)),
    Err(c) => Err(c.map(unbox)),
  }
}

fn downcast_env<R: 'static>(env: &AnyEnv) -> &R {
  match (**env).downcast_ref::<R>() {
    Some(r) => r,
    None => panic!("kernel_a: environment type mismatch (kernel bug)"),
  }
}

fn downcast_env_arc<R: Send + Sync + 'static>(env: &AnyEnv) -> Arc<R> {
  match Arc::clone(env).downcast::<R>() {
    Ok(r) => r,
    Err(_) => panic!("kernel_a: environment type mismatch (kernel bug)"),
  }
}

// ── constructors ────────────────────────────────────────────────────────────────────────────

/// A ready success.
pub fn succeed<A, E, R>(a: A) -> Effect<A, E, R> {
  Effect {
    node: Node::Pure(Ok(a)),
  }
}

/// A ready typed failure.
pub fn fail<A, E, R>(e: E) -> Effect<A, E, R> {
  Effect {
    node: Node::Pure(Err(Cause::Fail(e))),
  }
}

/// A defect.
pub fn die<A, E, R>(msg: impl Into<String>) -> Effect<A, E, R> {
  Effect {
    node: Node::Pure(Err(Cause::Die(Defect::new(msg)))),
  }
}

/// Defer building an effect until it runs.
pub fn suspend<A, E, R>(f: impl FnOnce() -> Effect<A, E, R> + Send + 'static) -> Effect<A, E, R> {
  Effect {
    node: Node::Sync(Box::new(move |_| f()), 1),
  }
}

/// A lazy synchronous computation.
pub fn sync<A, E, R>(f: impl FnOnce() -> A + Send + 'static) -> Effect<A, E, R> {
  Effect {
    node: Node::Sync(Box::new(move |_| succeed(f())), 1),
  }
}

/// A real async step: the future must be `Send + 'static` (it may not borrow the env).
pub fn from_async<A, E, R: Send + Sync + 'static, F, Fut>(f: F) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  F: FnOnce() -> Fut + Send + 'static,
  Fut: Future<Output = Result<A, E>> + Send + 'static,
{
  // One allocation: the boxed async block is lazy (`f` runs on first poll), so it needs no
  // separate builder closure.
  Effect::from_instr(Instr::Future(Box::pin(async move {
    erase_exit(f().await.map_err(Cause::Fail))
  })))
}

/// Sleep on the executor's timer.
pub fn sleep<E: Send + 'static, R: Send + Sync + 'static>(d: Duration) -> Effect<(), E, R> {
  Effect::from_instr(Instr::Async(Box::new(move |_, fiber| {
    let s = fiber.executor().sleep(d);
    Box::pin(async move {
      s.await;
      Ok(erase(()))
    })
  })))
}

/// Yield once to the executor.
pub fn yield_now<E: Send + 'static, R: Send + Sync + 'static>() -> Effect<(), E, R> {
  Effect::from_instr(Instr::Async(Box::new(|_, _| {
    Box::pin(async {
      YieldOnce(false).await;
      Ok(erase(()))
    })
  })))
}

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

/// Read capability `T` (combinator style; `effect!` spells it `~T`).
pub fn need<T>() -> Need<T> {
  Need(PhantomData)
}

/// See [`need`]. The index `I` is a method parameter so `need::<T>()` needs no other turbofish.
pub struct Need<T>(PhantomData<fn() -> T>);

impl<T: Send + Sync + 'static> Need<T> {
  pub fn effect<E, R, I>(self) -> Effect<Arc<T>, E, R>
  where
    R: Selector<T, I>,
  {
    Effect {
      node: Node::Sync(
        Box::new(|r: &R| succeed(Arc::clone(Selector::<T, I>::get(r)))),
        1,
      ),
    }
  }

  pub fn map<B, E, R, I>(self, f: impl FnOnce(Arc<T>) -> B + Send + 'static) -> Effect<B, E, R>
  where
    R: Selector<T, I>,
  {
    Effect {
      node: Node::Sync(
        Box::new(move |r: &R| succeed(f(Arc::clone(Selector::<T, I>::get(r))))),
        1,
      ),
    }
  }

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

// ── Ctx and `~` binds ───────────────────────────────────────────────────────────────────────

/// What an `effect!` body owns: the env (shared) and the current fiber. Clone = 2 refcount bumps.
pub struct Ctx<R> {
  env: Arc<R>,
  fiber: Arc<FiberContext>,
}

impl<R> Clone for Ctx<R> {
  fn clone(&self) -> Self {
    Self {
      env: Arc::clone(&self.env),
      fiber: Arc::clone(&self.fiber),
    }
  }
}

impl<R: Send + Sync + 'static> Ctx<R> {
  pub(crate) fn new_root(env: Arc<R>, fiber: Arc<FiberContext>) -> Self {
    Self { env, fiber }
  }

  pub(crate) fn env_arc(&self) -> AnyEnv {
    Arc::clone(&self.env) as AnyEnv
  }

  pub fn env(&self) -> &R {
    &self.env
  }

  pub fn fiber(&self) -> &Arc<FiberContext> {
    &self.fiber
  }

  /// Capability `T` at the inferred index.
  pub fn get<T, I>(&self) -> &Arc<T>
  where
    R: Selector<T, I>,
  {
    Selector::<T, I>::get(&*self.env)
  }

  /// Run a sub-effect whose environment is a subset of `R` on this fiber (what `~eff` expands
  /// to). A `Die` or `Interrupt` of the sub-effect resumes as a panic that the enclosing runloop
  /// converts back into the same `Cause`, so `?` only ever sees the typed error.
  pub fn bind<B, E2, Sub, Is>(&self, eff: Effect<B, E2, Sub>) -> BindFuture<B, E2>
  where
    B: Send + 'static,
    E2: Send + 'static,
    Sub: HList,
    R: Embed<Sub, Is>,
  {
    let mut node = eff.node;
    let mut sub: Option<Sub> = None;
    loop {
      match node {
        Node::Pure(r) => return BindFuture(BindState::Ready(Some(r))),
        Node::Sync(f, _) => {
          let s = sub.get_or_insert_with(|| Embed::<Sub, Is>::project(&*self.env));
          node = f(s).node;
        }
        node @ Node::Op(_) => {
          let env: AnyEnv = Arc::new(sub.unwrap_or_else(|| Embed::<Sub, Is>::project(&*self.env)));
          let instr = Effect { node }.into_instr();
          return BindFuture(BindState::Running(
            Box::new(Runloop::new(instr, env, Arc::clone(&self.fiber))),
            PhantomData,
          ));
        }
      }
    }
  }
}

/// Future of a `~` bind.
pub struct BindFuture<B, E>(BindState<B, E>);

enum BindState<B, E> {
  Ready(Option<Exit<B, E>>),
  Running(Box<Runloop>, PhantomData<fn() -> (B, E)>),
}

impl<B: 'static, E: 'static> Unpin for BindFuture<B, E> {}

fn surface<B, E>(exit: Exit<B, E>) -> Result<B, E> {
  match exit {
    Ok(b) => Ok(b),
    Err(Cause::Fail(e)) => Err(e),
    Err(Cause::Die(d)) => resume_unwind(d.into_payload()),
    Err(Cause::Interrupt(id)) => resume_unwind(Box::new(InterruptSignal(id))),
  }
}

impl<B: 'static, E: 'static> Future for BindFuture<B, E> {
  type Output = Result<B, E>;
  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<B, E>> {
    match &mut self.get_mut().0 {
      BindState::Ready(r) => Poll::Ready(surface(
        r.take().expect("kernel_a: bind polled after completion"),
      )),
      BindState::Running(rl, _) => Pin::new(&mut **rl)
        .poll(cx)
        .map(|exit| surface(unerase_exit(exit))),
    }
  }
}

// ── combinators ─────────────────────────────────────────────────────────────────────────────

impl<A, E, R> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  pub(crate) fn from_instr(i: Instr) -> Self {
    Self { node: Node::Op(i) }
  }

  fn sync_node(f: impl FnOnce(&R) -> Effect<A, E, R> + Send + 'static, depth: u16) -> Self {
    Self {
      node: Node::Sync(Box::new(f), depth),
    }
  }

  pub(crate) fn into_instr(self) -> Instr {
    match self.node {
      Node::Pure(r) => Instr::Pure(erase_exit(r)),
      Node::Sync(f, _) => Instr::Sync(Box::new(move |env| f(downcast_env::<R>(env)).into_instr())),
      Node::Op(i) => i,
    }
  }

  /// The `effect!` target: an async body that owns a `Ctx<R>` clone.
  pub fn from_ctx<F, Fut>(f: F) -> Self
  where
    F: FnOnce(Ctx<R>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<A, E>> + Send + 'static,
  {
    Self::from_instr(Instr::Async(Box::new(move |env, fiber| {
      let ctx = Ctx {
        env: downcast_env_arc::<R>(env),
        fiber: Arc::clone(fiber),
      };
      let fut = f(ctx);
      Box::pin(async move { erase_exit(fut.await.map_err(Cause::Fail)) })
    })))
  }

  pub fn map<B: Send + 'static>(self, f: impl FnOnce(A) -> B + Send + 'static) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(r) => Effect::sync_node(
        move |_| Effect {
          node: Node::Pure(r.map(f)),
        },
        1,
      ),
      Node::Sync(g, d) if d < FUSE_MAX => Effect::sync_node(move |r| g(r).map_now(f), d + 1),
      node => Effect::from_instr(
        Self { node }.seq(Box::new(move |v| Instr::Pure(Ok(erase(f(unbox::<A>(v))))))),
      ),
    }
  }

  fn map_now<B: Send + 'static>(self, f: impl FnOnce(A) -> B + Send + 'static) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(r) => Effect {
        node: Node::Pure(r.map(f)),
      },
      node => Self { node }.map(f),
    }
  }

  pub fn flat_map<B: Send + 'static>(
    self,
    f: impl FnOnce(A) -> Effect<B, E, R> + Send + 'static,
  ) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(Ok(a)) => Effect::sync_node(move |_| f(a), 1),
      Node::Pure(Err(c)) => Effect {
        node: Node::Pure(Err(c)),
      },
      Node::Sync(g, d) if d < FUSE_MAX => Effect::sync_node(move |r| g(r).flat_map_now(f), d + 1),
      node => {
        Effect::from_instr(Self { node }.seq(Box::new(move |v| f(unbox::<A>(v)).into_instr())))
      }
    }
  }

  /// Append an erased continuation: pushes onto an existing `Seq` (no box per step).
  fn seq(self, k: Box<dyn FnOnce(crate::runloop::AnyBox) -> Instr + Send>) -> Instr {
    match self.node {
      Node::Op(Instr::Seq(inner, mut ks)) => {
        ks.push(k);
        Instr::Seq(inner, ks)
      }
      node => Instr::Seq(Box::new(Self { node }.into_instr()), vec![k]),
    }
  }

  fn flat_map_now<B: Send + 'static>(
    self,
    f: impl FnOnce(A) -> Effect<B, E, R> + Send + 'static,
  ) -> Effect<B, E, R> {
    match self.node {
      Node::Pure(Ok(a)) => f(a),
      Node::Pure(Err(c)) => Effect {
        node: Node::Pure(Err(c)),
      },
      node => Self { node }.flat_map(f),
    }
  }

  /// Sequence, discarding this effect's value.
  pub fn then<B: Send + 'static>(self, next: Effect<B, E, R>) -> Effect<B, E, R> {
    self.flat_map(move |_| next)
  }

  pub fn map_err<E2: Send + 'static>(
    self,
    f: impl FnOnce(E) -> E2 + Send + 'static,
  ) -> Effect<A, E2, R> {
    match self.node {
      Node::Pure(r) => Effect::sync_node(
        move |_| Effect {
          node: Node::Pure(r.map_err(|c| c.map(f))),
        },
        1,
      ),
      node => Effect::from_instr(Instr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(move |exit| match exit {
          Ok(v) => Instr::Pure(Ok(v)),
          Err(c) => Instr::Pure(Err(c.map(|e| erase(f(unbox::<E>(e)))))),
        }),
      )),
    }
  }

  /// Recover from a typed failure (defects and interrupts pass through).
  pub fn catch<E2: Send + 'static>(
    self,
    f: impl FnOnce(E) -> Effect<A, E2, R> + Send + 'static,
  ) -> Effect<A, E2, R> {
    match self.node {
      Node::Pure(Ok(a)) => succeed(a),
      Node::Pure(Err(c)) => match c.retype::<E2>() {
        Ok(c) => Effect {
          node: Node::Pure(Err(c)),
        },
        Err(e) => Effect::sync_node(move |_| f(e), 1),
      },
      node => Effect::from_instr(Instr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(move |exit| match exit {
          Ok(v) => Instr::Pure(Ok(v)),
          Err(Cause::Fail(e)) => f(unbox::<E>(e)).into_instr(),
          Err(other) => Instr::Pure(Err(other)),
        }),
      )),
    }
  }

  /// Observe the full outcome as a value (Effect.ts `Effect.exit`).
  pub fn exit<E2: Send + 'static>(self) -> Effect<Exit<A, E>, E2, R> {
    match self.node {
      Node::Pure(r) => succeed(r),
      node => Effect::from_instr(Instr::Fold(
        Box::new(Self { node }.into_instr()),
        Box::new(|exit| Instr::Pure(Ok(erase(unerase_exit::<A, E>(exit))))),
      )),
    }
  }

  /// Run `fin` after this effect however it ends (success, failure, defect or interrupt);
  /// `fin` runs uninterruptibly.
  pub fn ensuring(self, fin: Effect<(), Never, R>) -> Self {
    Self::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
      let m = fiber.mask();
      Instr::Mask(
        Box::new(Instr::Fold(
          Box::new(Instr::Mask(Box::new(self.into_instr()), MaskOp::Set(m))),
          Box::new(move |exit| {
            Instr::FlatMap(
              Box::new(fin.into_instr()),
              Box::new(move |_| Instr::Pure(exit)),
            )
          }),
        )),
        MaskOp::Inc,
      )
    })))
  }

  /// Interrupts are deferred until this effect finishes.
  pub fn uninterruptible(self) -> Self {
    Self::from_instr(Instr::Mask(Box::new(self.into_instr()), MaskOp::Inc))
  }

  // ── environment ──

  /// Provide capability `T`, eliminating it from `R` (index inferred).
  pub fn provide<T, I>(self, t: T) -> Effect<A, E, <R as Plucker<T, I>>::Rest>
  where
    T: Send + Sync + 'static,
    R: Plucker<T, I>,
  {
    match self.node {
      Node::Pure(r) => Effect {
        node: Node::Pure(r),
      },
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |outer| {
          let rest = downcast_env::<R::Rest>(outer).clone();
          Arc::new(R::plant(Arc::new(t), rest)) as AnyEnv
        }),
      )),
    }
  }

  /// Provide a whole sub-environment at once (`Sculptor`).
  pub fn provide_some<Sub, Is>(self, sub: Sub) -> Effect<A, E, <R as Sculptor<Sub, Is>>::Rest>
  where
    Sub: Send + 'static,
    R: Sculptor<Sub, Is>,
  {
    match self.node {
      Node::Pure(r) => Effect {
        node: Node::Pure(r),
      },
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |outer| {
          let rest = downcast_env::<<R as Sculptor<Sub, Is>>::Rest>(outer).clone();
          Arc::new(R::plant(sub, rest)) as AnyEnv
        }),
      )),
    }
  }

  /// Provide exactly `R`.
  pub fn provide_all(self, env: R) -> Effect<A, E, HNil> {
    match self.node {
      Node::Pure(r) => Effect {
        node: Node::Pure(r),
      },
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |_| Arc::new(env) as AnyEnv),
      )),
    }
  }

  /// Run against any environment value that contains `R` (the edge: a flat app env built once).
  pub fn provide_env<Env, Is>(self, env: Env) -> Effect<A, E, HNil>
  where
    Env: Embed<R, Is> + Send + 'static,
    R: HList,
  {
    match self.node {
      Node::Pure(r) => Effect {
        node: Node::Pure(r),
      },
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(move |_| Arc::new(env.project()) as AnyEnv),
      )),
    }
  }

  /// Lift into a wider environment (`~` does this implicitly).
  pub fn narrow<Wide, Is>(self) -> Effect<A, E, Wide>
  where
    Wide: Embed<R, Is> + HList,
    R: HList,
  {
    match self.node {
      Node::Pure(r) => Effect {
        node: Node::Pure(r),
      },
      Node::Sync(f, d) => Effect {
        node: Node::Sync(Box::new(move |w: &Wide| f(&w.project()).narrow()), d),
      },
      node => Effect::from_instr(Instr::Provide(
        Box::new(Self { node }.into_instr()),
        Box::new(|outer| Arc::new(downcast_env::<Wide>(outer).project()) as AnyEnv),
      )),
    }
  }

  // ── fibers ──

  /// Fork as a child of the current fiber (interrupted and awaited when the parent exits).
  pub fn fork(self) -> Effect<FiberHandle<A, E>, Never, R> {
    Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
      let (ctx, cell) = spawn_fiber(
        self.into_instr(),
        Arc::clone(env),
        Some(fiber),
        fiber.executor(),
      );
      Instr::Pure(Ok(erase(FiberHandle::<A, E>::new(ctx, cell))))
    })))
  }

  /// Fork with no parent (survives the forking fiber).
  pub fn fork_daemon(self) -> Effect<FiberHandle<A, E>, Never, R> {
    Effect::from_instr(Instr::WithFiber(Box::new(move |env, fiber| {
      let (ctx, cell) = spawn_fiber(self.into_instr(), Arc::clone(env), None, fiber.executor());
      Instr::Pure(Ok(erase(FiberHandle::<A, E>::new(ctx, cell))))
    })))
  }

  pub fn zip<B: Send + 'static>(self, other: Effect<B, E, R>) -> Effect<(A, B), E, R> {
    self.flat_map(move |a| other.map(move |b| (a, b)))
  }
}

impl<A: Send + 'static, R: Send + Sync + 'static> Effect<A, Never, R> {
  /// An infallible effect under any error type.
  pub fn widen<E: Send + 'static>(self) -> Effect<A, E, R> {
    self.map_err(|n| match n {})
  }
}

impl<A: Send + 'static, E: Send + 'static> Effect<A, E, HNil> {
  /// Run on the default runtime, parking this thread (never inside an async task).
  pub fn block_on(self) -> Exit<A, E> {
    Runtime::global().block_on(self)
  }

  /// `!Send` view of this effect.
  pub fn local(self) -> crate::local::LocalEffect<A, E, HNil> {
    crate::local::LocalEffect::from_effect(self)
  }
}

/// `uninterruptible_mask(|restore| ..)`: the body is uninterruptible except inside `restore(..)`.
pub fn uninterruptible_mask<A, E, R>(
  f: impl FnOnce(Restore) -> Effect<A, E, R> + Send + 'static,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let m = fiber.mask();
    Instr::Mask(Box::new(f(Restore(m)).into_instr()), MaskOp::Inc)
  })))
}

/// Restores the interruptibility outside an `uninterruptible_mask`.
#[derive(Clone, Copy, Debug)]
pub struct Restore(u32);

impl Restore {
  pub fn apply<A, E, R>(self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    Effect::from_instr(Instr::Mask(Box::new(eff.into_instr()), MaskOp::Set(self.0)))
  }
}

/// Acquire uninterruptibly, use (interruptibly), release uninterruptibly however `use_` ends.
pub fn acquire_use_release<T, B, E, R>(
  acquire: Effect<T, E, R>,
  use_: impl FnOnce(T) -> Effect<B, E, R> + Send + 'static,
  release: impl FnOnce(T, ExitKind) -> Effect<(), Never, R> + Send + 'static,
) -> Effect<B, E, R>
where
  T: Clone + Send + 'static,
  B: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_instr(Instr::WithFiber(Box::new(move |_, fiber| {
    let m = fiber.mask();
    Instr::Mask(
      Box::new(Instr::FlatMap(
        Box::new(acquire.into_instr()),
        Box::new(move |t| {
          let t: T = unbox(t);
          let t2 = t.clone();
          Instr::Fold(
            Box::new(Instr::Mask(Box::new(use_(t).into_instr()), MaskOp::Set(m))),
            Box::new(move |exit| {
              let kind = match &exit {
                Ok(_) => ExitKind::Success,
                Err(Cause::Fail(_)) => ExitKind::Failure,
                Err(Cause::Die(_)) => ExitKind::Die,
                Err(Cause::Interrupt(_)) => ExitKind::Interrupt,
              };
              Instr::FlatMap(
                Box::new(release(t2, kind).into_instr()),
                Box::new(move |_| Instr::Pure(exit)),
              )
            }),
          )
        }),
      )),
      MaskOp::Inc,
    )
  })))
}
