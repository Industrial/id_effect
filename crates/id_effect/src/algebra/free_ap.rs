//! Free applicative functor over [`Effect`].
//!
//! [`FreeAp`] collects effectful arguments without committing to an interpretation strategy.
//! [`FreeAp::interpret`] runs independent [`FreeAp::Lift`] nodes sequentially; callers can
//! swap in a parallel interpreter when effects commute.

use crate::kernel::{Effect, succeed};

/// Free applicative over [`Effect<A, E, R>`].
pub enum FreeAp<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  /// Pure value with no pending effects.
  Pure(A),
  /// Lift a single effect into the free applicative.
  Lift(Effect<A, E, R>),
  /// Type-erased binary apply node.
  Ap2(Box<dyn FnOnce() -> Effect<A, E, R>>),
}

impl<A, E, R> std::fmt::Debug for FreeAp<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Pure(_) => f.debug_tuple("Pure").field(&"...").finish(),
      Self::Lift(_) => f.debug_tuple("Lift").field(&"...").finish(),
      Self::Ap2(_) => f.write_str("Ap2(...)"),
    }
  }
}

impl<A, E, R> FreeAp<A, E, R>
where
  A: 'static,
  E: 'static,
  R: 'static,
{
  /// Lift a pure value.
  #[inline]
  pub fn pure(value: A) -> Self {
    Self::Pure(value)
  }

  /// Lift an effect.
  #[inline]
  pub fn lift(effect: Effect<A, E, R>) -> Self {
    Self::Lift(effect)
  }

  /// Binary applicative apply: `f` is applied to the results of `fb` and `fc`.
  #[inline]
  pub fn ap2<B, C>(f: fn(B, C) -> A, fb: FreeAp<B, E, R>, fc: FreeAp<C, E, R>) -> Self
  where
    B: 'static,
    C: 'static,
  {
    Self::Ap2(Box::new(move || {
      fb.interpret()
        .flat_map(move |b| fc.interpret().map(move |c| f(b, c)))
    }))
  }

  /// Functorial map over the final value.
  #[inline]
  pub fn map<B>(self, f: fn(A) -> B) -> FreeAp<B, E, R>
  where
    B: 'static,
  {
    match self {
      Self::Pure(a) => FreeAp::Pure(f(a)),
      other => FreeAp::Lift(other.interpret().map(f)),
    }
  }

  /// Interpret the free applicative into a concrete [`Effect`].
  pub fn interpret(self) -> Effect<A, E, R> {
    match self {
      Self::Pure(a) => succeed(a),
      Self::Lift(eff) => eff,
      Self::Ap2(run) => run(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Exit, pure, run_test};

  #[test]
  fn pure_interprets_to_success() {
    let exit: Exit<i32, ()> = run_test(FreeAp::<i32, (), ()>::pure(9).interpret(), ());
    assert_eq!(exit, Exit::succeed(9));
  }

  #[test]
  fn ap2_combines_two_lifts() {
    let free = FreeAp::ap2(
      |a: i32, b: i32| a + b,
      FreeAp::lift(pure(2)),
      FreeAp::lift(pure(3)),
    );
    let exit: Exit<i32, ()> = run_test(free.interpret(), ());
    assert_eq!(exit, Exit::succeed(5));
  }

  #[test]
  fn map_transforms_pure_value() {
    let free = FreeAp::pure(4).map(|n| n * 2);
    let exit: Exit<i32, ()> = run_test(free.interpret(), ());
    assert_eq!(exit, Exit::succeed(8));
  }
}
