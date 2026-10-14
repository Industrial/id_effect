//! Bind narrower [`Effect`] values inside wider environments (shared [`Env`](super::env::Env)).

use super::env::Env;
use super::set::{CapKeys, CapList, CapabilitySet, FromEnv};
use crate::kernel::effect::{BoxFuture, Effect, box_future};
use std::panic::Location;

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
#[diagnostic::on_unimplemented(
  message = "`~` cannot bind `{Self}` inside an `effect!` whose environment is `{Wide}`",
  label = "cannot bind this step in `{Wide}`",
  note = "`~` binds an `Effect<_, _, R>` where `R` is `()`, `Env`, or a `caps!(…)` list, or a `Result`",
  note = "if `R` cannot be inferred, annotate the step's type (e.g. `let x: Effect<_, _, caps!(…)> = …`)"
)]
pub trait CapBind<'a, Wide> {
  /// Success value type of the bound effect.
  type A;
  /// Error type of the bound effect.
  type E;
  /// Run `self` against `wide`, narrowing the environment when required.
  fn cap_bind(self, wide: &'a mut Wide) -> BoxFuture<'a, Result<Self::A, Self::E>>;

  /// Like [`Self::cap_bind`], with the `~` call site used in missing-capability panics.
  ///
  /// The default ignores `at`; the narrowing impl reports it.
  #[inline]
  fn cap_bind_at(
    self,
    wide: &'a mut Wide,
    at: &'static Location<'static>,
  ) -> BoxFuture<'a, Result<Self::A, Self::E>>
  where
    Self: Sized,
  {
    let _ = at;
    self.cap_bind(wide)
  }
}

/// Environment-free effects (`R = ()`) bind in **any** environment — the environment is ignored.
///
/// This is what lets `~logger.info("…")` (an `Effect<(), _, ()>`) appear in an `effect!` body
/// whose environment is `caps!(…)` or `Env` without a turbofish.
impl<'a, Wide, A, E> CapBind<'a, Wide> for Effect<A, E, ()>
where
  A: 'a,
  E: 'a,
{
  type A = A;
  type E = E;

  #[inline]
  fn cap_bind(self, _wide: &'a mut Wide) -> BoxFuture<'a, Result<A, E>> {
    box_future(async move { self.run(&mut ()).await })
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
  #[track_caller]
  fn cap_bind(self, wide: &'a mut Wide) -> BoxFuture<'a, Result<A, E>> {
    self.cap_bind_at(wide, Location::caller())
  }

  fn cap_bind_at(
    self,
    wide: &'a mut Wide,
    at: &'static Location<'static>,
  ) -> BoxFuture<'a, Result<A, E>> {
    box_future(async move {
      let env = wide.bind_env().clone();
      if let Err(err) = R::verify(&env) {
        panic!(
          "{err}; required by `~` at {at} (narrowing to `{}`)",
          std::any::type_name::<R>()
        );
      }
      let mut narrow = R::from_env(env);
      self.run(&mut narrow).await
    })
  }
}

/// Bind helper used by the `effect!` macro (`~` desugars to this).
///
/// `#[track_caller]`: a missing capability while narrowing panics naming the `~` call site.
#[inline]
#[track_caller]
pub fn cap_into_bind<'a, Wide, T>(t: T, wide: &'a mut Wide) -> BoxFuture<'a, Result<T::A, T::E>>
where
  T: CapBind<'a, Wide>,
{
  t.cap_bind_at(wide, Location::caller())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Cap;
  use crate::kernel::succeed;
  use pollster::FutureExt;

  #[test]
  fn cap_bind_unit_env_runs_effect() {
    let eff = succeed::<i32, (), ()>(9);
    let mut r = ();
    let out = eff.cap_bind(&mut r).block_on().unwrap();
    assert_eq!(out, 9);
  }

  #[test]
  fn cap_bind_result_value() {
    let eff: Result<i32, ()> = Ok(3);
    let mut wide = Env::new();
    let out = eff.cap_bind(&mut wide).block_on().unwrap();
    assert_eq!(out, 3);
  }

  #[test]
  fn cap_into_bind_runs_through_cap_list() {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Bind(pub u32);
    let mut env = Env::new();
    env.insert::<Cap<Bind>>(Bind(5u32));
    let mut caps = CapList::<(Cap<Bind>,)>::from_env(env);
    let eff = succeed::<u32, (), CapList<(Cap<Bind>,)>>(caps.get::<Cap<Bind>>().0);
    let out = cap_into_bind(eff, &mut caps).block_on().unwrap();
    assert_eq!(out, 5);

    let eff =
      Effect::new(|caps: &mut CapList<(Cap<Bind>,)>| Ok::<u32, ()>(caps.get::<Cap<Bind>>().0));
    assert_eq!(eff.cap_bind(&mut caps).block_on().unwrap(), 5);
  }
}
