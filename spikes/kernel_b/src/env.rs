//! Typed `R` as trait bounds (design §2.1): `Has<T>`, `Replace<T>`, `Env`, `Service`, and the
//! `IntoBind` lift used by `effect!`'s `~ expr`.

use std::future::Future;

use crate::cause::Cause;
use crate::kernel::{Effect, LocalEffect, LocalRunFut, RunFut, unit_env};

/// The environment provides capability `T`. `need::<T>()` / `~T` compile only when this holds, so
/// a missing capability is a compile error, never a runtime panic (D5).
#[diagnostic::on_unimplemented(
  message = "effect environment `{Self}` does not provide capability `{T}`",
  label = "`{T}` is required here",
  note = "add a `{T}` field to your `#[derive(Env)]` struct, or add `Has<{T}>` to the `R: …` bounds"
)]
pub trait Has<T>: Send + Sync + 'static {
  fn get(&self) -> &T;
}

/// The environment has a `T` slot that can be swapped (`provide_service`).
#[diagnostic::on_unimplemented(
  message = "effect environment `{Self}` has no `{T}` slot to replace",
  label = "`provide_service` needs an existing `{T}` slot",
  note = "`provide_service` overrides a slot of exactly the slot type; pass a value of the env field's type (newtypes and `Arc<dyn ..>` slots must match exactly)"
)]
pub trait Replace<T>: Has<T> {
  fn replace(&mut self, value: T) -> T;
}

/// A complete application environment (`#[derive(Env)]`). `()` deliberately does **not**
/// implement `Env`: that keeps the env-free `IntoBind` impl coherent next to the same-`R` one.
pub trait Env: Clone + Send + Sync + 'static {}

/// The bound every capability meets (same as 0.6 `Needs`, `needs.rs:19`).
pub trait Service: Clone + Send + Sync + 'static {}

impl<T: Clone + Send + Sync + 'static> Service for T {}

/// A single service is its own one-capability environment (`hello().provide(Greeting(..))`).
/// Spike addition: the design only gets `Has` from the derive.
impl<T: Service> Has<T> for T {
  #[inline]
  fn get(&self) -> &T {
    self
  }
}

impl<T: Service> Replace<T> for T {
  #[inline]
  fn replace(&mut self, value: T) -> T {
    std::mem::replace(self, value)
  }
}

/// Read capability `T` (cloned, like 0.6 `need`). Generic over `R`, so it needs an expected type.
pub fn need<T: Service, E: Send + 'static, R: Has<T>>() -> Effect<T, E, R> {
  Effect::sync(|r: &mut R| Ok(r.get().clone()))
}

/// Lift used by `~ expr` inside `effect!`: an effect over the same `R`, or an env-free one.
pub trait IntoBind<R> {
  type Out;
  type Err;
  type Fut<'a>: Future<Output = Result<Self::Out, Cause<Self::Err>>> + 'a
  where
    R: 'a;
  fn into_bind(self, r: &mut R) -> Self::Fut<'_>;
}

impl<A: Send + 'static, E: Send + 'static, R: Send + 'static> IntoBind<R> for Effect<A, E, R> {
  type Out = A;
  type Err = E;
  type Fut<'a>
    = RunFut<'a, A, E, R>
  where
    R: 'a;

  #[inline]
  fn into_bind(self, r: &mut R) -> Self::Fut<'_> {
    self.run_in(r)
  }
}

/// Coherent next to the impl above only because `(): !Env` (design §2.4, `CapBindR` trick).
impl<A: Send + 'static, E: Send + 'static, R: Env> IntoBind<R> for Effect<A, E, ()> {
  type Out = A;
  type Err = E;
  type Fut<'a>
    = RunFut<'static, A, E, ()>
  where
    R: 'a;

  #[inline]
  fn into_bind(self, _r: &mut R) -> Self::Fut<'_> {
    self.run_in(unit_env())
  }
}

impl<A: 'static, E: 'static, R: 'static> IntoBind<R> for LocalEffect<A, E, R> {
  type Out = A;
  type Err = E;
  type Fut<'a>
    = LocalRunFut<'a, A, E, R>
  where
    R: 'a;

  #[inline]
  fn into_bind(self, r: &mut R) -> Self::Fut<'_> {
    self.run_in(r)
  }
}

impl<A: 'static, E: 'static, R: Env> IntoBind<R> for LocalEffect<A, E, ()> {
  type Out = A;
  type Err = E;
  type Fut<'a>
    = LocalRunFut<'static, A, E, ()>
  where
    R: 'a;

  #[inline]
  fn into_bind(self, _r: &mut R) -> Self::Fut<'_> {
    self.run_in(unit_env())
  }
}

/// `effect!` codegen target for `~ expr`. Not public API.
#[doc(hidden)]
#[inline]
pub fn __bind<X: IntoBind<R>, R>(x: X, r: &mut R) -> X::Fut<'_> {
  x.into_bind(r)
}

/// `effect!` codegen for the error path of `~ expr`: widen `Cause<E1>` into the body's
/// `Cause<E>` through `E1: Into<E>`, or, for an infallible `Cause<Never>`, into any `E`
/// (autoref dispatch; `Never: Into<AppError>` cannot be written for a foreign `AppError`).
#[doc(hidden)]
pub mod __widen {
  use std::marker::PhantomData;

  use crate::cause::{Cause, Never};
  use crate::kernel::{Effect, LocalEffect};

  /// The bound effect's error type, read off the effect *before* `IntoBind` picks an impl (the
  /// `R` of a generic helper call is still an inference variable then; `E` is not).
  /// Also carries the success type `A`, so a bound value's type is known immediately (method
  /// calls on it do not wait for `IntoBind` impl selection).
  pub struct Tag<E, A = ()>(PhantomData<fn() -> (E, A)>);

  impl<E, A> Tag<E, A> {
    #[inline]
    pub fn out(&self, v: A) -> A {
      v
    }
  }

  pub trait ErrTag {
    type E;
    type A;
    fn __kb_err_tag(&self) -> Tag<Self::E, Self::A> {
      Tag(PhantomData)
    }
  }

  impl<A, E, R> ErrTag for Effect<A, E, R> {
    type E = E;
    type A = A;
  }

  impl<A, E, R> ErrTag for LocalEffect<A, E, R> {
    type E = E;
    type A = A;
  }

  pub trait ViaNever {
    fn __kb_widen<E2>(&self, c: Cause<Never>) -> Cause<E2>;
  }

  impl<A> ViaNever for &Tag<Never, A> {
    fn __kb_widen<E2>(&self, c: Cause<Never>) -> Cause<E2> {
      c.absurd()
    }
  }

  pub trait ViaInto<E1> {
    fn __kb_widen<E2>(&self, c: Cause<E1>) -> Cause<E2>
    where
      E1: Into<E2>;
  }

  impl<E1, A> ViaInto<E1> for Tag<E1, A> {
    fn __kb_widen<E2>(&self, c: Cause<E1>) -> Cause<E2>
    where
      E1: Into<E2>,
    {
      c.widen()
    }
  }
}

/// `caps!(pub trait OrderCaps = Db + Mailer);` — a trait alias over `Env + Has<..>` (design
/// §2.1; in 0.6 `caps!` was a *type*).
#[macro_export]
macro_rules! caps {
  ($vis:vis trait $name:ident = $($cap:ident)++ ;) => {
    $vis trait $name: $crate::Env $(+ $crate::Has<$cap>)+ {}
    impl<T: $crate::Env $(+ $crate::Has<$cap>)+> $name for T {}
  };
}
