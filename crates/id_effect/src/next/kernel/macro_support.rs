//! Support items for the `*_next!` macro expansions, reached as `next::__private`. Not API.
//!
//! - [`Lift`] with [`LiftNever`] / [`LiftFrom`]: error widening for `~` binds by autoref
//!   specialisation (spike A `lib.rs:46-84`). `(&&Lift::new(e)).lift()` picks `LiftNever` for a
//!   `Never` error (widens to anything) and `LiftFrom` otherwise (`E: From<E2>`).
//! - [`bind_local`]: what `~expr` expands to inside `effect_local_next!`.
//! - [`CapsChecked`]: the identity projection `caps_next!` wraps a de-duplicated list in when it
//!   also reports a duplicate (so the error does not cascade).

use std::cell::Cell;
use std::future::Future;
use std::sync::Arc;

use super::anybox::AnyEnv;
use super::bind::surface;
use super::cause::from_exit;
use super::ctx::Ctx;
use super::local::IntoLocal;
use crate::next::capability::Embed;
use crate::runtime::Never;

/// A `~` bind's error, waiting to be widened into the body's error type.
pub struct Lift<E>(Cell<Option<E>>);

impl<E> Lift<E> {
  /// Wraps the sub-effect's error.
  pub fn new(e: E) -> Self {
    Self(Cell::new(Some(e)))
  }
}

/// Widening of a `Never` error (picked first by autoref).
pub trait LiftNever {
  /// Any `E` (a `Never` has no values).
  fn lift<E>(&self) -> E;
}

impl LiftNever for &Lift<Never> {
  fn lift<E>(&self) -> E {
    match self.0.take() {
      Some(n) => match n {},
      None => unreachable!("next: `~` error lifted twice (macro bug)"),
    }
  }
}

/// Widening through `From` (every other error type).
pub trait LiftFrom<E2> {
  /// `E::from(e2)`.
  fn lift<E: From<E2>>(&self) -> E;
}

impl<E2> LiftFrom<E2> for Lift<E2> {
  fn lift<E: From<E2>>(&self) -> E {
    match self.0.take() {
      Some(e) => E::from(e),
      None => unreachable!("next: `~` error lifted twice (macro bug)"),
    }
  }
}

/// `~sub` inside `effect_local_next!`: runs a [`LocalEffect`](super::LocalEffect) or a `Send`
/// [`Effect`](super::Effect) (via [`IntoLocal`]) whose environment `Sub` is a part of `R`, on
/// `ctx`'s fiber, as a nested local runloop. Outcomes surface as in [`Ctx::bind`]: the typed error
/// as `Err`, a defect or an interrupt re-raised in the enclosing runloop.
pub fn bind_local<R, X, A, E, Sub, Is>(
  ctx: &Ctx<R>,
  sub: X,
) -> impl Future<Output = Result<A, E>> + use<R, X, A, E, Sub, Is>
where
  X: IntoLocal<A, E, Sub>,
  R: Embed<Sub, Is> + Send + Sync + 'static,
  Sub: Send + Sync + 'static,
  A: 'static,
  E: 'static,
{
  // Same environment: share it. Otherwise project the subset (one `Arc` clone per cap).
  let env: Arc<Sub> = match (Arc::clone(&ctx.env) as AnyEnv).downcast::<Sub>() {
    Ok(same) => same,
    Err(_) => Arc::new(Embed::<Sub, Is>::project(&*ctx.env)),
  };
  let run = sub
    .into_local()
    .run_in(Ctx::new(env, Arc::clone(&ctx.fiber)));
  async move { surface(from_exit(run.await)) }
}

/// The identity on `Self`, with a const argument that carries `caps_next!`'s duplicate errors.
pub trait CapsChecked<const N: usize> {
  /// `Self`.
  type Out;
}

impl<T, const N: usize> CapsChecked<N> for T {
  type Out = T;
}
