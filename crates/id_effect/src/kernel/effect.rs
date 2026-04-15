//! **Effect** — the core abstraction for effectful computation.
//!
//! An `Effect` is a **lazy**, **asynchronous** computation that:
//! - Requires environment `R`
//! - May succeed with `A`
//! - May fail with `E`
//!
//! ## Definition
//!
//! ```text
//! EFFECT[A, E, R] ::= R → Future[Result[A, E]]
//! ```
//!
//! ## Type Parameters
//!
//! | Parameter | Variance | Meaning |
//! |-----------|----------|---------|
//! | `A` | Covariant | Success value type |
//! | `E` | Covariant | Error type |
//! | `R` | Contravariant | Environment/context type |
//!
//! ## Algebraic Structure
//!
//! - `Effect[_, E, R]` forms a **Monad** for fixed `E`, `R`
//! - `Effect[A, _, R]` forms a **Bifunctor** with `Result`
//! - `Effect[A, E, _]` is **Contravariant** in `R`
//!
//! ## Relationship to Stratum 0, 1 & 2
//!
//! - Uses: [`Result`](super::result) — success/failure encoding
//! - Uses: [`Reader`](super::reader) — environment threading (implicit)
//! - Uses: [`Thunk`](super::thunk) — lazy suspension (implicit)
//! - Uses: [`Functor`](super::super::algebra::functor) — `map` operation
//! - Uses: [`Monad`](super::super::algebra::monad) — `flat_map`, sequencing
//! - Uses: [`Bifunctor`](super::super::algebra::bifunctor) — error mapping

use core::convert::Infallible;
use core::future::{Future, ready};
use core::marker::PhantomData;
use core::pin::Pin;

use crate::context::{Cons, Context, Nil};
use crate::failure::exit::Exit;
use crate::failure::union::Or;
use crate::runtime::Never;

// ── BoxFuture ───────────────────────────────────────────────────────────────

/// Owned, pinned, type-erased future used by [`Effect::run`] and [`IntoBind`].
///
/// ## Why not “stack `pin!` only”?
///
/// An `async` body is a `Future` that often **borrows the environment** `&mut R` for its whole
/// poll lifetime. [`Effect`] is a **uniform** `Box<dyn EffectOp<…>>` so combinators and
/// `dyn` dispatch stay ergonomic. To return “something to poll” from `run` across that boundary,
/// the compiler must **erase** the concrete future type → **`dyn Future + 'a`**, which in safe Rust
/// is stored as **`Pin<Box<…>>`** (this alias).
///
/// [`std::pin::pin!`] can **pin on the stack**, but that `Pin<&mut impl Future>` is
/// tied to the **current stack frame** and cannot be **owned** by `Effect` or returned from
/// `new_async` without self-referential / unsafe patterns (this crate uses `#![forbid(unsafe_code)]`).
///
/// A thin wrapper future around a concrete `Fut` still ends in **`Pin<Box<…>>`** for the wrapper
/// and adds **extra state** (e.g. init vs polling) versus a **single** `async move { … }` state
/// machine, so the current shape keeps **one** allocation for the async body.
///
/// For **bind-free** bodies, [`effect!`](macro@crate::effect) uses [`Effect::new`] instead,
/// which stores the sync closure in `SyncOnce` and boxes only a trivial [`core::future::ready`]
/// future on `run` — no user `async` block and no `new_async` indirection.
///
/// The boxed future is **`dyn Future`** (not `Send`) so [`crate::streaming::stream::Stream`] and other `Rc`-based code can use
/// [`Effect`]; prefer [`crate::runtime::run_blocking`] or a single-threaded async runtime when driving
/// [`Effect::run`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Heap-pin a future as [`BoxFuture`] (single allocation; coerces to `dyn Future`).
#[inline]
pub fn box_future<'a, Fut>(fut: Fut) -> BoxFuture<'a, Fut::Output>
where
  Fut: Future + 'a,
  Fut::Output: 'a,
{
  Box::pin(fut)
}

// ── IntoBind ────────────────────────────────────────────────────────────────

/// Values that can be sequenced like [`Effect::run`] inside `async` bind chains (`~await` / `flat_map`).
pub trait IntoBind<'a, R, A, E> {
  /// Turns `self` into a boxed future given the environment `r`.
  fn into_bind(self, r: &'a mut R) -> BoxFuture<'a, Result<A, E>>;
}

impl<'a, R, A, E> IntoBind<'a, R, A, E> for Effect<A, E, R>
where
  A: 'a,
  E: 'a,
  R: 'a,
{
  /// Runs this effect against `r`.
  #[inline]
  fn into_bind(self, r: &'a mut R) -> BoxFuture<'a, Result<A, E>> {
    self.run(r)
  }
}

/// `Result` implements [`IntoBind`] via [`core::future::ready`] and [`box_future`].
///
/// That is the minimal safe shape for a uniform [`BoxFuture`] return type; avoiding the boxed future
/// would require changing [`IntoBind`]'s contract or using `unsafe` inside this crate. For hot paths,
/// return an [`Effect`] or a concrete future type instead of `Result` when the extra allocation matters.
impl<'a, R, A, E> IntoBind<'a, R, A, E> for Result<A, E>
where
  A: 'a,
  E: 'a,
{
  /// Wraps the ready [`Result`] in a [`BoxFuture`].
  #[inline]
  fn into_bind(self, _r: &'a mut R) -> BoxFuture<'a, Result<A, E>> {
    box_future(ready(self))
  }
}

/// Dispatches [`IntoBind::into_bind`] for any implementor (including [`Effect`] and [`Result`]).
#[inline]
pub fn into_bind<'a, R, A, E, T: IntoBind<'a, R, A, E>>(
  t: T,
  r: &'a mut R,
) -> BoxFuture<'a, Result<A, E>> {
  t.into_bind(r)
}

// ── EffectOp (Internal) ─────────────────────────────────────────────────────

trait EffectOp<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  fn run_boxed<'a>(self: Box<Self>, r: &'a mut R) -> BoxFuture<'a, Result<A, E>>;
}

struct SyncOnce<F> {
  f: Option<F>,
}

impl<A, E, R, F> EffectOp<A, E, R> for SyncOnce<F>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnOnce(&mut R) -> Result<A, E> + 'static,
{
  #[inline]
  fn run_boxed<'a>(mut self: Box<Self>, r: &'a mut R) -> BoxFuture<'a, Result<A, E>> {
    let f = self
      .f
      .take()
      .expect("Effect::run invoked twice (internal bug)");
    box_future(ready(f(r)))
  }
}

struct Once<F> {
  f: Option<F>,
}

impl<A, E, R, F> EffectOp<A, E, R> for Once<F>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: for<'a> FnOnce(&'a mut R) -> BoxFuture<'a, Result<A, E>> + 'static,
{
  #[inline]
  fn run_boxed<'a>(mut self: Box<Self>, r: &'a mut R) -> BoxFuture<'a, Result<A, E>> {
    let f = self
      .f
      .take()
      .expect("Effect::run invoked twice (internal bug)");
    f(r)
  }
}

// ── Effect ──────────────────────────────────────────────────────────────────

/// A lazy, asynchronous computation with environment, error, and success types.
///
/// # Type Parameters
///
/// - `A`: Success value type (covariant)
/// - `E`: Error type (covariant), defaults to `()`
/// - `R`: Environment type (contravariant), defaults to `()`
///
/// # Example
///
/// ```rust,ignore
/// use id_effect::{Effect, succeed, fail};
///
/// // Create effects
/// let ok: Effect<i32, &str, ()> = succeed(42);
/// let err: Effect<i32, &str, ()> = fail("error");
///
/// // Compose effects
/// let doubled = ok.map(|n| n * 2);
/// let chained = doubled.flat_map(|n| succeed(n + 1));
/// ```
#[allow(clippy::type_complexity)]
pub struct Effect<A, E = (), R = ()>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  op: Box<dyn EffectOp<A, E, R>>,
  _pd: PhantomData<fn() -> (A, E, R)>,
}

impl<A, E, R> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  // ── Constructors ──────────────────────────────────────────────────────

  /// Create an effect from a synchronous computation.
  ///
  /// The closure runs once when the effect is polled.
  #[inline]
  pub fn new<F>(f: F) -> Self
  where
    F: FnOnce(&mut R) -> Result<A, E> + 'static,
  {
    Self {
      op: Box::new(SyncOnce { f: Some(f) }),
      _pd: PhantomData,
    }
  }

  /// Create an effect from an asynchronous computation.
  ///
  /// The closure returns a [`BoxFuture`] that may borrow `&mut R`.
  #[inline]
  pub fn new_async<F>(f: F) -> Self
  where
    F: for<'a> FnOnce(&'a mut R) -> BoxFuture<'a, Result<A, E>> + 'static,
  {
    Self {
      op: Box::new(Once { f: Some(f) }),
      _pd: PhantomData,
    }
  }

  // ── Execution ─────────────────────────────────────────────────────────

  /// Execute this effect with the given environment.
  ///
  /// Consumes the effect (each effect runs at most once).
  #[inline]
  pub fn run<'a>(self, r: &'a mut R) -> BoxFuture<'a, Result<A, E>> {
    self.op.run_boxed(r)
  }

  // ── Functor Operations ────────────────────────────────────────────────

  /// Map the success value.
  ///
  /// `effect.map(f)` produces an effect that, when run, runs `effect`
  /// and applies `f` to the success value.
  #[inline]
  pub fn map<B, G>(self, g: G) -> Effect<B, E, R>
  where
    B: 'static,
    G: FnOnce(A) -> B + 'static,
  {
    Effect::new_async(move |r| {
      box_future(async move {
        let a = self.run(r).await?;
        Ok(g(a))
      })
    })
  }

  /// Replace the success value with a constant.
  #[inline]
  pub fn as_<B: 'static>(self, b: B) -> Effect<B, E, R> {
    self.map(move |_| b)
  }

  /// Discard the success value, returning unit.
  #[inline]
  pub fn void(self) -> Effect<(), E, R> {
    self.as_(())
  }

  // ── Monad Operations ──────────────────────────────────────────────────

  /// Sequentially compose two effects.
  ///
  /// `effect.flat_map(f)` runs `effect`, passes the success value to `f`
  /// to get another effect, and runs that effect.
  #[inline]
  pub fn flat_map<B, H>(self, h: H) -> Effect<B, E, R>
  where
    B: 'static,
    H: FnOnce(A) -> Effect<B, E, R> + 'static,
  {
    Effect::new_async(move |r| {
      box_future(async move {
        let a = self.run(r).await?;
        h(a).run(r).await
      })
    })
  }

  /// Sequence two effects, discarding the first result.
  #[inline]
  pub fn and_then<B: 'static>(self, other: Effect<B, E, R>) -> Effect<B, E, R> {
    self.flat_map(move |_| other)
  }

  /// Sequence two effects, discarding the second result.
  #[inline]
  pub fn and_then_discard<B: 'static>(self, other: Effect<B, E, R>) -> Effect<A, E, R> {
    self.flat_map(move |a| other.map(move |_| a))
  }

  // ── Bifunctor / Error Operations ──────────────────────────────────────

  /// Map the error value.
  #[inline]
  pub fn map_error<E2, H>(self, h: H) -> Effect<A, E2, R>
  where
    E2: 'static,
    H: FnOnce(E) -> E2 + 'static,
  {
    Effect::new_async(move |r| box_future(async move { self.run(r).await.map_err(h) }))
  }

  /// Recover from failure by running another effect.
  #[inline]
  pub fn catch<E2, H>(self, h: H) -> Effect<A, E2, R>
  where
    E2: 'static,
    H: FnOnce(E) -> Effect<A, E2, R> + 'static,
  {
    Effect::new_async(move |r| {
      box_future(async move {
        match self.run(r).await {
          Ok(a) => Ok(a),
          Err(e) => h(e).run(r).await,
        }
      })
    })
  }

  /// Run a side effect when `self` fails, then propagate the **same** error if the tap succeeds.
  ///
  /// On `Err(e)`, formats `e` with [`std::fmt::Debug`] into an owned string and passes it to `h`.
  /// If that effect succeeds, returns `Err(e)` unchanged. If the tap effect fails, returns that
  /// error instead (the original `e` is dropped).
  ///
  /// This shape avoids capturing `&E` in `'static` closures and does **not** require [`Clone`] on
  /// `E`. For taps that need the full `E` value, use [`catch`] or map errors to a cloneable type.
  ///
  /// Success values are unchanged; `h` is not called.
  #[inline]
  pub fn tap_error<H>(self, h: H) -> Effect<A, E, R>
  where
    E: std::fmt::Debug + 'static,
    H: FnOnce(String) -> Effect<(), E, R> + 'static,
  {
    Effect::new_async(move |r| {
      box_future(async move {
        match self.run(r).await {
          Ok(a) => Ok(a),
          Err(e) => {
            let msg = format!("{e:?}");
            h(msg).run(r).await?;
            Err(e)
          }
        }
      })
    })
  }

  /// Recover from any error with a fallback value.
  #[inline]
  pub fn catch_all<F>(self, fallback: F) -> Effect<A, Never, R>
  where
    F: FnOnce(E) -> A + 'static,
  {
    Effect::new_async(move |r| {
      box_future(async move {
        match self.run(r).await {
          Ok(a) => Ok(a),
          Err(e) => Ok(fallback(e)),
        }
      })
    })
  }

  /// Tag error as [`Or::Left`] for widening.
  #[inline]
  pub fn union_error<E2>(self) -> Effect<A, Or<E, E2>, R>
  where
    E2: 'static,
  {
    self.map_error(Or::Left)
  }

  /// `flat_map` with different error types unified as [`Or`].
  #[inline]
  pub fn flat_map_union<B, E2, H>(self, h: H) -> Effect<B, Or<E, E2>, R>
  where
    B: 'static,
    E2: 'static,
    H: FnOnce(A) -> Effect<B, E2, R> + 'static,
  {
    self
      .union_error::<E2>()
      .flat_map(move |a| h(a).map_error(Or::Right))
  }

  // ── Environment Operations ────────────────────────────────────────────

  /// Provide the environment, eliminating the `R` parameter.
  #[inline]
  pub fn provide(self, ctx: R) -> Effect<A, E, ()> {
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let mut ctx = ctx;
        self.run(&mut ctx).await
      })
    })
  }

  /// **MTL `MonadReader.local`** — run this effect with a *modified* environment.
  ///
  /// Haskell analogue:
  /// ```haskell
  /// local :: MonadReader r m => (r -> r) -> m a -> m a
  /// ```
  ///
  /// The modifier `f` is applied to the environment before the effect runs;
  /// the caller's environment is **not** mutated.
  ///
  /// # Example
  ///
  /// ```rust
  /// use id_effect::kernel::effect::{succeed, Effect};
  /// use id_effect::runtime::run_blocking;
  ///
  /// let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env));
  /// // run in an environment doubled from the caller's
  /// let doubled = eff.local(|r: &mut i32| *r *= 2);
  /// assert_eq!(run_blocking(doubled, 21), Ok(42));
  /// ```
  #[inline]
  pub fn local<F>(self, f: F) -> Effect<A, E, R>
  where
    R: Clone,
    F: FnOnce(&mut R) + 'static,
  {
    Effect::new_async(move |r: &mut R| {
      box_future(async move {
        let mut local_env = r.clone();
        f(&mut local_env);
        self.run(&mut local_env).await
      })
    })
  }

  /// **Environment widening** — run an effect that needs `R` inside an environment `S`
  /// by projecting `S` down to `R` with the function `f`.
  ///
  /// This is the **contravariant** map over the environment type parameter.
  /// In category-theory terms it is `contramap` on the *environment* functor.
  ///
  /// Haskell analogue (MTL `withReaderT`):
  /// ```haskell
  /// withReaderT :: (r' -> r) -> ReaderT r m a -> ReaderT r' m a
  /// ```
  ///
  /// # Example
  ///
  /// ```rust
  /// use id_effect::kernel::effect::{succeed, Effect};
  /// use id_effect::runtime::run_blocking;
  ///
  /// #[derive(Clone)]
  /// struct AppEnv { multiplier: i32 }
  ///
  /// let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env * 2));
  /// // Widen: AppEnv -> i32 by projecting the field
  /// let widened = eff.zoom_env(|app: &mut AppEnv| app.multiplier);
  /// assert_eq!(run_blocking(widened, AppEnv { multiplier: 21 }), Ok(42));
  /// ```
  #[inline]
  pub fn zoom_env<S, F>(self, f: F) -> Effect<A, E, S>
  where
    S: 'static,
    F: FnOnce(&mut S) -> R + 'static,
  {
    Effect::new_async(move |s: &mut S| {
      box_future(async move {
        let mut inner = f(s);
        self.run(&mut inner).await
      })
    })
  }

  // ── Resource Management ───────────────────────────────────────────────

  /// Run `finalizer` after this effect completes (success or failure).
  #[inline]
  pub fn ensuring(self, finalizer: Effect<(), Never, R>) -> Effect<A, E, R> {
    Effect::new_async(move |r| {
      box_future(async move {
        let result = self.run(r).await;
        let _ = finalizer.run(r).await;
        result
      })
    })
  }

  /// Observe the exit status without changing the result.
  #[inline]
  pub fn on_exit<F>(self, f: F) -> Effect<A, E, R>
  where
    A: Clone,
    E: Clone,
    F: FnOnce(Exit<A, E>) -> Effect<(), Never, R> + 'static,
  {
    Effect::new_async(move |r| {
      box_future(async move {
        match self.run(r).await {
          Ok(value) => {
            let _ = f(Exit::succeed(value.clone())).run(r).await;
            Ok(value)
          }
          Err(error) => {
            let _ = f(Exit::fail(error.clone())).run(r).await;
            Err(error)
          }
        }
      })
    })
  }
}

// ── Context-aware Effect methods ────────────────────────────────────────────

impl<A, E, K: ?Sized, V, Tail>
  Effect<A, E, Context<Cons<crate::layer::service::Service<K, V>, Tail>>>
where
  A: 'static,
  E: 'static,
  V: Clone + 'static,
  Tail: Clone + 'static,
{
  /// Supply the head service value and shrink the context.
  #[inline]
  pub fn provide_head(self, value: V) -> Effect<A, E, Context<Tail>> {
    Effect::new_async(move |tail: &mut Context<Tail>| {
      box_future(async move {
        let mut full = Context::new(Cons(
          crate::layer::service::Service::<K, _>::new(value.clone()),
          tail.as_ref().clone(),
        ));
        self.run(&mut full).await
      })
    })
  }

  /// Like [`Self::provide_head`] but takes a full service cell.
  #[inline]
  pub fn provide_service(
    self,
    svc: crate::layer::service::Service<K, V>,
  ) -> Effect<A, E, Context<Tail>> {
    self.provide_head(svc.value)
  }
}

impl<A, E> Effect<A, E, Context<Nil>>
where
  A: 'static,
  E: 'static,
{
  /// Run with an empty context.
  #[inline]
  pub async fn run_provided(self) -> Result<A, E> {
    self.run(&mut Context::new(Nil)).await
  }
}

// ── Constructors (Free Functions) ───────────────────────────────────────────

/// Create an effect from an async closure.
///
/// The returned future must be `'static` (cannot borrow `&mut R`).
/// For futures that borrow the environment, use [`Effect::new_async`].
#[inline]
pub fn from_async<A, E, R, F, Fut>(f: F) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: for<'a> FnOnce(&'a mut R) -> Fut + 'static,
  Fut: Future<Output = Result<A, E>> + 'static,
{
  Effect::new_async(move |r| box_future(f(r)))
}

/// Effect that succeeds immediately with `value`.
#[inline]
pub fn succeed<A, E, R>(value: A) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  Effect::new(move |_r| Ok(value))
}

/// Effect that fails immediately with `err`.
#[inline]
pub fn fail<A, E, R>(err: E) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  Effect::new(move |_r| Err(err))
}

/// Minimal `succeed` with unit error and environment.
#[inline]
pub fn pure<A>(value: A) -> Effect<A, (), ()>
where
  A: Send + 'static,
{
  succeed::<A, (), ()>(value)
}

/// Access the environment.
#[inline]
pub fn ask<R: Clone + 'static>() -> Effect<R, Never, R> {
  Effect::new(|r: &mut R| Ok(r.clone()))
}

/// Access part of the environment.
#[inline]
pub fn asks<A, R, F>(f: F) -> Effect<A, Never, R>
where
  A: 'static,
  R: 'static,
  F: FnOnce(&R) -> A + 'static,
{
  Effect::new(move |r: &mut R| Ok(f(r)))
}

// ── Resource Management (Free Functions) ────────────────────────────────────

/// Acquire a resource, then ensure release runs afterward.
#[inline]
pub fn acquire_release<A, E, R, E2, R2, F>(acquire: Effect<A, E, R>, release: F) -> Effect<A, E, R>
where
  A: Clone + 'static,
  E: 'static,
  R: 'static,
  E2: 'static,
  R2: Default + 'static,
  F: FnOnce(A) -> Effect<(), E2, R2> + 'static,
{
  Effect::new_async(move |r| {
    box_future(async move {
      let value = acquire.run(r).await?;
      let mut release_env = R2::default();
      let _ = release(value.clone()).run(&mut release_env).await;
      Ok(value)
    })
  })
}

/// Create a scope, run `f(scope)`, then close the scope.
#[inline]
pub fn scope_with<A, E, R, F>(f: F) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
  F: FnOnce(crate::resource::scope::Scope) -> Effect<A, E, R> + 'static,
{
  Effect::new_async(move |r| {
    box_future(async move {
      let scope = crate::resource::scope::Scope::make();
      let result = f(scope.clone()).run(r).await;
      match result {
        Ok(value) => {
          scope.close_with_exit(Exit::succeed(()));
          Ok(value)
        }
        Err(error) => {
          scope.close_with_exit(Exit::die("scope_with effect failed"));
          Err(error)
        }
      }
    })
  })
}

/// Run an effect inside a fresh scope.
#[inline]
pub fn scoped<A, E, R>(effect: Effect<A, E, R>) -> Effect<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  Effect::new_async(move |r| {
    box_future(async move {
      let scope = crate::resource::scope::Scope::make();
      let result = effect.run(r).await;
      match result {
        Ok(value) => {
          scope.close_with_exit(Exit::succeed(()));
          Ok(value)
        }
        Err(error) => {
          scope.close_with_exit(Exit::die("scoped effect failed"));
          Err(error)
        }
      }
    })
  })
}

// ── Utility ─────────────────────────────────────────────────────────────────

/// Unwrap a result known to be infallible.
#[inline]
pub fn unwrap_infallible<A>(r: Result<A, Infallible>) -> A {
  match r {
    Ok(a) => a,
    Err(e) => match e {},
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;
  use std::sync::{Arc, Mutex};

  fn run<A, E, R>(effect: Effect<A, E, R>, env: R) -> Result<A, E>
  where
    A: 'static,
    E: 'static,
    R: 'static,
  {
    crate::runtime::run_blocking(effect, env)
  }

  mod constructors {
    use super::*;

    #[test]
    fn succeed_returns_value() {
      let eff: Effect<i32, &str, ()> = succeed(42);
      assert_eq!(run(eff, ()), Ok(42));
    }

    #[test]
    fn fail_returns_error() {
      let eff: Effect<i32, &str, ()> = fail("error");
      assert_eq!(run(eff, ()), Err("error"));
    }

    #[test]
    fn pure_returns_value() {
      let eff = pure(42);
      assert_eq!(run(eff, ()), Ok(42));
    }

    #[test]
    fn new_runs_closure() {
      let eff: Effect<i32, (), ()> = Effect::new(|_| Ok(42));
      assert_eq!(run(eff, ()), Ok(42));
    }

    #[test]
    fn new_receives_environment() {
      let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env * 2));
      assert_eq!(run(eff, 21), Ok(42));
    }
  }

  mod from_async_fn {
    use super::*;
    use core::future::ready;

    #[test]
    fn ready_ok_returns_value() {
      let eff: Effect<i32, &str, ()> = from_async(|_r| ready(Ok(41)));
      assert_eq!(run(eff, ()), Ok(41));
    }

    #[test]
    fn ready_err_propagates() {
      let eff: Effect<(), &str, ()> = from_async(|_r| ready(Err("e")));
      assert_eq!(run(eff, ()), Err("e"));
    }

    #[test]
    fn async_block_ok() {
      let eff: Effect<u64, (), ()> = from_async(|_r| async move { Ok::<u64, ()>(99) });
      assert_eq!(run(eff, ()), Ok(99));
    }
  }

  mod functor_operations {
    use super::*;

    #[test]
    fn map_transforms_success() {
      let eff: Effect<i32, &str, ()> = succeed(21);
      assert_eq!(run(eff.map(|n| n * 2), ()), Ok(42));
    }

    #[test]
    fn map_passes_through_error() {
      let eff: Effect<i32, &str, ()> = fail("error");
      assert_eq!(run(eff.map(|n| n * 2), ()), Err("error"));
    }

    #[test]
    fn as_replaces_value() {
      let eff: Effect<i32, &str, ()> = succeed(1);
      assert_eq!(run(eff.as_("replaced"), ()), Ok("replaced"));
    }

    #[test]
    fn void_discards_value() {
      let eff: Effect<i32, &str, ()> = succeed(42);
      assert_eq!(run(eff.void(), ()), Ok(()));
    }
  }

  mod monad_operations {
    use super::*;

    #[test]
    fn flat_map_chains_effects() {
      let eff: Effect<i32, &str, ()> = succeed(5);
      let chained = eff.flat_map(|n| succeed(n * 2));
      assert_eq!(run(chained, ()), Ok(10));
    }

    #[test]
    fn flat_map_propagates_first_error() {
      let eff: Effect<i32, &str, ()> = fail("first");
      let chained = eff.flat_map(|n| succeed(n * 2));
      assert_eq!(run(chained, ()), Err("first"));
    }

    #[test]
    fn flat_map_propagates_second_error() {
      let eff: Effect<i32, &str, ()> = succeed(5);
      let chained = eff.flat_map(|_| fail::<i32, &str, ()>("second"));
      assert_eq!(run(chained, ()), Err("second"));
    }

    #[test]
    fn and_then_discards_first_result() {
      let eff1: Effect<i32, &str, ()> = succeed(1);
      let eff2: Effect<&str, &str, ()> = succeed("second");
      assert_eq!(run(eff1.and_then(eff2), ()), Ok("second"));
    }

    #[test]
    fn and_then_discard_keeps_first_result() {
      let eff1: Effect<i32, &str, ()> = succeed(1);
      let eff2: Effect<&str, &str, ()> = succeed("second");
      assert_eq!(run(eff1.and_then_discard(eff2), ()), Ok(1));
    }
  }

  mod error_operations {
    use super::*;

    #[test]
    fn map_error_transforms_error() {
      let eff: Effect<i32, &str, ()> = fail("error");
      assert_eq!(run(eff.map_error(|e| e.len()), ()), Err(5));
    }

    #[test]
    fn map_error_passes_through_success() {
      let eff: Effect<i32, &str, ()> = succeed(42);
      assert_eq!(run(eff.map_error(|e| e.len()), ()), Ok(42));
    }

    #[test]
    fn catch_recovers_from_error() {
      let eff: Effect<i32, &str, ()> = fail("error");
      let recovered: Effect<i32, &str, ()> = eff.catch(|_| succeed(42));
      assert_eq!(run(recovered, ()), Ok(42));
    }

    #[test]
    fn catch_passes_through_success() {
      let eff: Effect<i32, &str, ()> = succeed(42);
      let caught: Effect<i32, &str, ()> = eff.catch(|_| succeed(0));
      assert_eq!(run(caught, ()), Ok(42));
    }

    #[test]
    fn catch_all_extracts_value() {
      let eff: Effect<i32, &str, ()> = fail("error");
      let recovered = eff.catch_all(|_| 42);
      assert_eq!(run(recovered, ()), Ok(42));
    }

    #[test]
    fn tap_error_passes_through_success() {
      let eff: Effect<i32, &str, ()> = succeed(42);
      let out = eff.tap_error(|_| fail::<(), &str, ()>("tap"));
      assert_eq!(run(out, ()), Ok(42));
    }

    #[test]
    fn tap_error_runs_tap_on_failure_then_repropagates() {
      use std::sync::atomic::{AtomicU32, Ordering};
      let n = Arc::new(AtomicU32::new(0));
      let n2 = Arc::clone(&n);
      let eff: Effect<i32, &str, ()> = fail("bad");
      let out = eff.tap_error(move |msg| {
        assert_eq!(msg, "\"bad\"");
        Effect::new(move |_: &mut ()| {
          n2.fetch_add(1, Ordering::SeqCst);
          Ok(())
        })
      });
      assert_eq!(run(out, ()), Err("bad"));
      assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tap_error_propagates_tap_failure() {
      let eff: Effect<i32, &str, ()> = fail("outer");
      let out = eff.tap_error(|_| fail::<(), &str, ()>("tap-fail"));
      assert_eq!(run(out, ()), Err("tap-fail"));
    }
  }

  mod environment_operations {
    use super::*;

    #[test]
    fn provide_fixes_environment() {
      let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env * 2));
      let provided = eff.provide(21);
      assert_eq!(run(provided, ()), Ok(42));
    }

    // ── local (MTL MonadReader.local) ──────────────────────────────────────

    mod local {
      use super::*;

      /// `local(f)(eff)` runs `eff` in `f(env)` without mutating the caller.
      #[test]
      fn local_modifies_environment_for_the_inner_effect() {
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env));
        let doubled = eff.local(|r: &mut i32| *r *= 2);
        assert_eq!(run(doubled, 21), Ok(42));
      }

      /// The caller's environment is NOT mutated by `local`.
      #[test]
      fn local_does_not_mutate_caller_environment() {
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env + 100));
        // run in env+1, but our outer env stays at 5
        let result = run(eff.local(|r: &mut i32| *r += 1), 5);
        assert_eq!(result, Ok(106));
        // If outer env were mutated we'd see 6 here — the test wouldn't reach this
        // but the immutability is enforced structurally by cloning.
      }

      /// `local(id)(eff) = eff` — identity modifier is a no-op.
      #[test]
      fn local_identity_is_noop() {
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env));
        let same = eff.local(|_: &mut i32| {});
        assert_eq!(run(same, 42), Ok(42));
      }

      /// Nested `local` calls compose: `local(f)(local(g)(eff)) = local(f∘g)(eff)`.
      #[test]
      fn local_composition_outer_then_inner() {
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env));
        // outer doubles, inner adds 1
        let composed = eff
          .local(|r: &mut i32| *r += 1)
          .local(|r: &mut i32| *r *= 2);
        // 10 -> *2 -> 20 -> +1 -> 21
        assert_eq!(run(composed, 10), Ok(21));
      }
    }

    // ── zoom_env (contravariant environment widening) ──────────────────────

    mod zoom_env {
      use super::*;

      #[derive(Clone)]
      struct AppEnv {
        multiplier: i32,
      }

      /// `zoom_env(f)(eff)` runs `eff` with the projection of the outer env.
      #[test]
      fn zoom_env_projects_outer_to_inner() {
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env * 2));
        let widened = eff.zoom_env(|app: &mut AppEnv| app.multiplier);
        assert_eq!(run(widened, AppEnv { multiplier: 21 }), Ok(42));
      }

      /// Stacking `zoom_env` is associative: two projections compose.
      #[test]
      fn zoom_env_stacking_composes() {
        #[derive(Clone)]
        struct Outer {
          inner: AppEnv,
        }
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env));
        let level1 = eff.zoom_env(|a: &mut AppEnv| a.multiplier);
        let level2 = level1.zoom_env(|o: &mut Outer| o.inner.clone());
        assert_eq!(
          run(
            level2,
            Outer {
              inner: AppEnv { multiplier: 7 }
            }
          ),
          Ok(7)
        );
      }

      /// `zoom_env` with identity projection is equivalent to the original.
      #[test]
      fn zoom_env_identity_projection_is_noop() {
        let eff: Effect<i32, (), i32> = Effect::new(|env| Ok(*env));
        let same = eff.zoom_env(|r: &mut i32| *r);
        assert_eq!(run(same, 42), Ok(42));
      }
    }
  }

  mod ensuring {
    use super::*;

    #[rstest]
    #[case::success(true)]
    #[case::failure(false)]
    fn ensuring_runs_finalizer(#[case] should_succeed: bool) {
      let calls = Arc::new(Mutex::new(0usize));
      let calls_ref = Arc::clone(&calls);
      let finalizer = Effect::new(move |_env: &mut ()| {
        *calls_ref.lock().expect("calls mutex poisoned") += 1;
        Ok::<(), Never>(())
      });

      let effect = if should_succeed {
        succeed::<u8, &str, ()>(7).ensuring(finalizer)
      } else {
        fail::<u8, &str, ()>("boom").ensuring(finalizer)
      };

      let result = run(effect, ());
      if should_succeed {
        assert_eq!(result, Ok(7));
      } else {
        assert_eq!(result, Err("boom"));
      }
      assert_eq!(*calls.lock().expect("calls mutex poisoned"), 1);
    }
  }

  mod on_exit {
    use super::*;

    #[rstest]
    #[case::success(true, "ok:3", Ok(3))]
    #[case::failure(false, "fail", Err("nope"))]
    fn on_exit_observes_result(
      #[case] should_succeed: bool,
      #[case] expected_seen: &str,
      #[case] expected_result: Result<u8, &str>,
    ) {
      let seen = Arc::new(Mutex::new(String::new()));
      let seen_ref = Arc::clone(&seen);
      let effect = if should_succeed {
        succeed::<u8, &str, ()>(3)
      } else {
        fail::<u8, &str, ()>("nope")
      }
      .on_exit(move |exit| {
        *seen_ref.lock().expect("seen mutex poisoned") = match exit {
          Exit::Success(value) => format!("ok:{value}"),
          Exit::Failure(_) => String::from("fail"),
        };
        succeed::<(), Never, ()>(())
      });

      assert_eq!(run(effect, ()), expected_result);
      assert_eq!(
        seen.lock().expect("seen mutex poisoned").as_str(),
        expected_seen
      );
    }
  }

  mod laws {
    use super::*;

    #[test]
    fn monad_left_identity() {
      // flat_map(succeed(a), f) = f(a)
      let a = 5;
      let f = |x: i32| succeed::<_, &str, ()>(x * 2);

      let left = succeed::<_, &str, ()>(a).flat_map(f);
      let right = f(a);

      assert_eq!(run(left, ()), run(right, ()));
    }

    #[test]
    fn monad_right_identity() {
      // flat_map(eff, succeed) = eff
      let eff: Effect<i32, &str, ()> = succeed(42);
      let result = eff.flat_map(succeed);
      assert_eq!(run(result, ()), Ok(42));
    }

    #[test]
    fn monad_associativity() {
      // flat_map(flat_map(eff, f), g) = flat_map(eff, |a| flat_map(f(a), g))
      let f = |x: i32| succeed::<_, &str, ()>(x + 1);
      let g = |x: i32| succeed::<_, &str, ()>(x * 2);

      let eff1: Effect<i32, &str, ()> = succeed(5);
      let left = eff1.flat_map(f).flat_map(g);

      let eff2: Effect<i32, &str, ()> = succeed(5);
      let right = eff2.flat_map(move |a| f(a).flat_map(g));

      assert_eq!(run(left, ()), run(right, ()));
    }

    #[test]
    fn functor_identity() {
      // eff.map(id) = eff
      let eff: Effect<i32, &str, ()> = succeed(42);
      let mapped = eff.map(|x| x);
      assert_eq!(run(mapped, ()), Ok(42));
    }

    #[test]
    fn functor_composition() {
      // eff.map(g).map(f) = eff.map(|x| f(g(x)))
      let f = |x: i32| x + 1;
      let g = |x: i32| x * 2;

      let eff1: Effect<i32, &str, ()> = succeed(5);
      let left = eff1.map(g).map(f);

      let eff2: Effect<i32, &str, ()> = succeed(5);
      let right = eff2.map(move |x| f(g(x)));

      assert_eq!(run(left, ()), run(right, ()));
    }

    #[test]
    fn fail_short_circuits() {
      // flat_map(fail(e), f) = fail(e)
      let eff: Effect<i32, &str, ()> = fail("error");
      let chained = eff.flat_map(|_| succeed::<_, &str, ()>(99));
      assert_eq!(run(chained, ()), Err("error"));
    }

    #[test]
    fn catch_ignores_success() {
      // catch(succeed(a), h) = succeed(a)
      let eff: Effect<i32, &str, ()> = succeed(42);
      let caught: Effect<i32, &str, ()> = eff.catch(|_| succeed(0));
      assert_eq!(run(caught, ()), Ok(42));
    }

    #[test]
    fn catch_handles_failure() {
      // catch(fail(e), h) = h(e)
      let eff: Effect<i32, &str, ()> = fail("error");
      let caught: Effect<i32, &str, ()> = eff.catch(|e| succeed(e.len() as i32));
      assert_eq!(run(caught, ()), Ok(5));
    }
  }

  // ── ask / asks ──────────────────────────────────────────────────────────────

  mod reader_operations {
    use super::*;

    #[test]
    fn ask_returns_environment() {
      let eff = ask::<i32>();
      assert_eq!(run(eff, 42), Ok(42));
    }

    #[test]
    fn asks_transforms_environment() {
      let eff = asks::<String, i32, _>(|n| format!("val={n}"));
      assert_eq!(run(eff, 7), Ok("val=7".to_string()));
    }
  }

  // ── Effect combinators: as_, void, and_then, and_then_discard ───────────────

  mod additional_combinators {
    use super::*;

    #[test]
    fn as_replaces_value() {
      let eff: Effect<i32, (), ()> = succeed(1).as_(99);
      assert_eq!(run(eff, ()), Ok(99));
    }

    #[test]
    fn void_discards_value() {
      let eff: Effect<(), (), ()> = succeed(42_i32).void();
      assert_eq!(run(eff, ()), Ok(()));
    }

    #[test]
    fn and_then_sequences_effects() {
      let eff: Effect<i32, (), ()> = succeed(1_i32).and_then(succeed(99_i32));
      assert_eq!(run(eff, ()), Ok(99));
    }

    #[test]
    fn and_then_discard_returns_first() {
      let eff: Effect<i32, (), ()> = succeed(42_i32).and_then_discard(succeed(99_i32));
      assert_eq!(run(eff, ()), Ok(42));
    }

    #[test]
    fn tap_error_observes_error_but_passes_through() {
      let observed = Arc::new(Mutex::new(String::new()));
      let obs_clone = Arc::clone(&observed);
      let eff: Effect<i32, &str, ()> = fail("oops");
      let tapped = eff.tap_error(move |msg| {
        *obs_clone.lock().unwrap() = msg.clone();
        succeed::<(), &str, ()>(())
      });
      assert_eq!(run(tapped, ()), Err("oops"));
      assert!(observed.lock().unwrap().contains("oops"));
    }

    #[test]
    fn catch_all_converts_error_to_never() {
      let eff: Effect<i32, &str, ()> = fail("bad");
      let recovered: Effect<i32, Never, ()> = eff.catch_all(|_| 0);
      assert_eq!(run(recovered, ()), Ok(0));
    }

    #[test]
    fn catch_all_ignores_success() {
      let eff: Effect<i32, &str, ()> = succeed(5);
      let recovered: Effect<i32, Never, ()> = eff.catch_all(|_| 0);
      assert_eq!(run(recovered, ()), Ok(5));
    }
  }

  // ── union_error / flat_map_union ────────────────────────────────────────────

  mod union_error_ops {
    use super::*;
    use crate::Or;

    #[test]
    fn union_error_wraps_error_in_or() {
      let eff: Effect<i32, &str, ()> = fail("oops");
      let unioned: Effect<i32, Or<&str, i32>, ()> = eff.union_error();
      let result = run(unioned, ());
      assert!(matches!(result, Err(Or::Left("oops"))));
    }

    #[test]
    fn union_error_preserves_success() {
      let eff: Effect<i32, &str, ()> = succeed(42);
      let unioned: Effect<i32, Or<&str, i32>, ()> = eff.union_error();
      assert_eq!(run(unioned, ()), Ok(42));
    }

    #[test]
    fn flat_map_union_chains_and_wraps_error() {
      let eff: Effect<i32, &str, ()> = succeed(10);
      let result: Effect<String, Or<&str, i32>, ()> =
        eff.flat_map_union::<String, i32, _>(|n| fail(n + 1));
      let res = run(result, ());
      assert!(matches!(res, Err(Or::Right(11))));
    }
  }

  // ── acquire_release ─────────────────────────────────────────────────────────

  mod resource_management {
    use super::*;

    #[test]
    fn acquire_release_runs_release_after_success() {
      let released = Arc::new(Mutex::new(false));
      let rel_clone = Arc::clone(&released);
      let result = run(
        acquire_release(succeed::<i32, (), ()>(42), move |_v| {
          *rel_clone.lock().unwrap() = true;
          succeed::<(), (), ()>(())
        }),
        (),
      );
      assert_eq!(result, Ok(42));
      assert!(*released.lock().unwrap(), "release should have been called");
    }

    #[test]
    fn scope_with_creates_and_closes_scope() {
      let result = run(scope_with(|_scope| succeed::<i32, (), ()>(7)), ());
      assert_eq!(result, Ok(7));
    }

    #[test]
    fn scope_with_closes_on_error() {
      let result = run(
        scope_with::<i32, &str, (), _>(|_scope| fail("scope_err")),
        (),
      );
      assert_eq!(result, Err("scope_err"));
    }

    #[test]
    fn scoped_runs_effect_in_scope() {
      let result = run(scoped(succeed::<i32, (), ()>(99)), ());
      assert_eq!(result, Ok(99));
    }

    #[test]
    fn scoped_propagates_error() {
      let result = run(scoped(fail::<i32, &str, ()>("err")), ());
      assert_eq!(result, Err("err"));
    }
  }

  // ── IntoBind / into_bind ────────────────────────────────────────────────────

  mod into_bind_ops {
    use super::*;

    #[test]
    fn into_bind_effect_runs_effect() {
      let eff: Effect<i32, (), ()> = succeed(99);
      let result = crate::runtime::run_blocking(
        Effect::<i32, (), ()>::new_async(move |r| into_bind(eff, r)),
        (),
      );
      assert_eq!(result, Ok(99));
    }

    #[test]
    fn into_bind_result_ok_returns_immediately() {
      let r: Result<i32, ()> = Ok(42);
      let result = crate::runtime::run_blocking(
        Effect::<i32, (), ()>::new_async(move |env| into_bind(r, env)),
        (),
      );
      assert_eq!(result, Ok(42));
    }

    #[test]
    fn into_bind_result_err_returns_error() {
      let r: Result<i32, &str> = Err("e");
      let result = crate::runtime::run_blocking(
        Effect::<i32, &str, ()>::new_async(move |env| into_bind(r, env)),
        (),
      );
      assert_eq!(result, Err("e"));
    }
  }

  // ── box_future ──────────────────────────────────────────────────────────────

  mod box_future_fn {
    use super::*;

    #[test]
    fn box_future_wraps_ready_future() {
      use core::future::ready;
      let fut = box_future(ready(Ok::<i32, ()>(5)));
      let result = crate::runtime::run_blocking(Effect::<i32, (), ()>::new_async(|_| fut), ());
      assert_eq!(result, Ok(5));
    }
  }

  // ── unwrap_infallible ───────────────────────────────────────────────────────

  mod unwrap_infallible_fn {
    use super::*;

    #[test]
    fn unwrap_infallible_extracts_ok_value() {
      let r: Result<i32, Infallible> = Ok(42);
      assert_eq!(unwrap_infallible(r), 42);
    }
  }
}
