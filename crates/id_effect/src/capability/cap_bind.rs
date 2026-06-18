//! Bind narrower [`Effect`] values inside wider environments (shared [`Env`](super::env::Env)).

use super::env::Env;
use super::set::{CapKeys, CapList, CapabilitySet, FromEnv};
use crate::kernel::effect::{BoxFuture, Effect, box_future};

/// Environments that expose a shared [`Env`] for [`CapBind`].
pub trait CapBindWide {
  /// Borrow the underlying [`Env`].
  fn bind_env(&self) -> &Env;
}

impl CapBindWide for Env {
  fn bind_env(&self) -> &Env {
    self
  }
}

impl<Ks: CapKeys> CapBindWide for CapList<Ks> {
  fn bind_env(&self) -> &Env {
    self.env()
  }
}

/// Non-unit capability environments for [`CapBind`].
pub trait CapBindR: FromEnv + CapabilitySet + CapBindWide {}

impl CapBindR for Env {}

impl<Ks: CapKeys> CapBindR for CapList<Ks> {}

/// Bind an [`Effect`] against a (possibly wider) runtime environment.
pub trait CapBind<'a, Wide> {
  /// Success value type of the bound effect.
  type A;
  /// Error type of the bound effect.
  type E;
  /// Run `self` against `wide`, narrowing the environment when required.
  fn cap_bind(self, wide: &'a mut Wide) -> BoxFuture<'a, Result<Self::A, Self::E>>;
}

impl<'a, A, E> CapBind<'a, ()> for Effect<A, E, ()>
where
  A: 'a,
  E: 'a,
{
  type A = A;
  type E = E;

  #[inline]
  fn cap_bind(self, r: &'a mut ()) -> BoxFuture<'a, Result<A, E>> {
    self.run(r)
  }
}

impl<'a, Wide, A, E> CapBind<'a, Wide> for Result<A, E>
where
  A: 'a,
  E: 'a,
{
  type A = A;
  type E = E;

  #[inline]
  fn cap_bind(self, _wide: &'a mut Wide) -> BoxFuture<'a, Result<A, E>> {
    use core::future::ready;
    box_future(ready(self))
  }
}

/// Clone shared [`Env`] from `wide`, verify inner keys, and run against narrowed `R`.
impl<'a, Wide, A, E, R> CapBind<'a, Wide> for Effect<A, E, R>
where
  Wide: CapBindWide + 'a,
  R: CapBindR + 'static,
  A: 'a,
  E: 'a,
{
  type A = A;
  type E = E;

  #[inline]
  fn cap_bind(self, wide: &'a mut Wide) -> BoxFuture<'a, Result<A, E>> {
    box_future(async move {
      let env = wide.bind_env().clone();
      R::verify(&env).expect("capability environment missing required keys");
      let mut narrow = R::from_env(env);
      self.run(&mut narrow).await
    })
  }
}

/// Bind helper used by the `effect!` macro (`~` desugars to this).
#[inline]
pub fn cap_into_bind<'a, Wide, T>(t: T, wide: &'a mut Wide) -> BoxFuture<'a, Result<T::A, T::E>>
where
  T: CapBind<'a, Wide>,
{
  t.cap_bind(wide)
}
