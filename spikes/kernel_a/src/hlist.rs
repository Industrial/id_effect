//! Typed `R`: the environment is an HList of `Arc`ed capabilities; lookups, provision, subset
//! projection and concatenation are traits whose index parameters the compiler infers
//! (design A §2.1).

use std::marker::PhantomData;
use std::sync::Arc;

/// The empty environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HNil;

/// One capability (shared, `Arc`) in front of the rest of the environment.
#[derive(Debug)]
pub struct HCons<H, T>(pub Arc<H>, pub T);

impl<H, T: Clone> Clone for HCons<H, T> {
  fn clone(&self) -> Self {
    Self(Arc::clone(&self.0), self.1.clone())
  }
}

impl<H, T> HCons<H, T> {
  pub fn new(head: H, tail: T) -> Self {
    Self(Arc::new(head), tail)
  }
}

/// Index witness: the capability is the head.
pub struct Here;
/// Index witness: the capability is somewhere in the tail.
pub struct There<I>(PhantomData<I>);

/// An environment value: cheap to clone (one `Arc` bump per capability), shareable across fibers.
pub trait HList: Clone + Send + Sync + 'static {
  const LEN: usize;
}

impl HList for HNil {
  const LEN: usize = 0;
}

impl<H: Send + Sync + 'static, T: HList> HList for HCons<H, T> {
  const LEN: usize = 1 + T::LEN;
}

/// Read capability `T` at the inferred index `I`. Missing `T` is a compile error.
#[diagnostic::on_unimplemented(
  message = "capability `{T}` is not in this effect's environment",
  label = "`{T}` is required here",
  note = "add `{T}` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running"
)]
pub trait Selector<T, I> {
  fn get(&self) -> &Arc<T>;
}

impl<T, Tl> Selector<T, Here> for HCons<T, Tl> {
  fn get(&self) -> &Arc<T> {
    &self.0
  }
}

#[diagnostic::do_not_recommend]
impl<T, H, Tl: Selector<T, I>, I> Selector<T, There<I>> for HCons<H, Tl> {
  fn get(&self) -> &Arc<T> {
    self.1.get()
  }
}

/// Remove capability `T` (at `I`) from the environment type: what `provide` eliminates.
#[diagnostic::on_unimplemented(
  message = "`{T}` is not required by this effect, so it cannot be provided",
  label = "this effect's environment `{Self}` has no `{T}`",
  note = "only capabilities in the effect's `R` can be provided; drop this `.provide(..)`"
)]
pub trait Plucker<T, I>: Sized {
  type Rest: HList;
  fn plant(t: Arc<T>, rest: Self::Rest) -> Self;
  fn pluck(self) -> (Arc<T>, Self::Rest);
}

impl<T: Send + Sync + 'static, Tl: HList> Plucker<T, Here> for HCons<T, Tl> {
  type Rest = Tl;
  fn plant(t: Arc<T>, rest: Tl) -> Self {
    Self(t, rest)
  }
  fn pluck(self) -> (Arc<T>, Tl) {
    (self.0, self.1)
  }
}

#[diagnostic::do_not_recommend]
impl<T, H: Send + Sync + 'static, Tl: Plucker<T, I>, I> Plucker<T, There<I>> for HCons<H, Tl> {
  type Rest = HCons<H, Tl::Rest>;
  fn plant(t: Arc<T>, rest: Self::Rest) -> Self {
    Self(rest.0, Tl::plant(t, rest.1))
  }
  fn pluck(self) -> (Arc<T>, Self::Rest) {
    let (t, rest) = self.1.pluck();
    (t, HCons(self.0, rest))
  }
}

/// Remove a whole sub-environment `Sub` (at indices `Is`): what `provide_some` eliminates.
#[diagnostic::on_unimplemented(
  message = "`{Sub}` is not a part of this effect's environment `{Self}`",
  label = "cannot provide `{Sub}` here"
)]
pub trait Sculptor<Sub, Is>: Sized {
  type Rest: HList;
  fn plant(sub: Sub, rest: Self::Rest) -> Self;
}

impl<S: HList> Sculptor<HNil, HNil> for S {
  type Rest = S;
  fn plant(_: HNil, rest: S) -> S {
    rest
  }
}

impl<S, H, T, I, Is> Sculptor<HCons<H, T>, HCons<I, Is>> for S
where
  S: Plucker<H, I>,
  S::Rest: Sculptor<T, Is>,
{
  type Rest = <S::Rest as Sculptor<T, Is>>::Rest;
  fn plant(sub: HCons<H, T>, rest: Self::Rest) -> Self {
    S::plant(sub.0, <S::Rest as Sculptor<T, Is>>::plant(sub.1, rest))
  }
}

/// Read-only projection of a sub-environment `Sub` out of `Self` (any subset, any order):
/// what narrowing (`~sub_effect`, `.narrow()`, `.provide_env(..)`) uses. Costs one `Arc` clone
/// per capability in `Sub`.
#[diagnostic::on_unimplemented(
  message = "the environment `{Self}` does not contain every capability of `{Sub}`",
  label = "cannot narrow `{Self}` to `{Sub}`"
)]
pub trait Embed<Sub, Is> {
  fn project(&self) -> Sub;
}

impl<S> Embed<HNil, HNil> for S {
  fn project(&self) -> HNil {
    HNil
  }
}

impl<S, H, T, I, Is> Embed<HCons<H, T>, HCons<I, Is>> for S
where
  S: Selector<H, I> + Embed<T, Is>,
{
  fn project(&self) -> HCons<H, T> {
    HCons(
      Arc::clone(Selector::<H, I>::get(self)),
      Embed::<T, Is>::project(self),
    )
  }
}

/// Type-level and value-level append (layer merge).
pub trait Concat<Rhs> {
  type Out: HList;
  fn concat(self, rhs: Rhs) -> Self::Out;
}

impl<Rhs: HList> Concat<Rhs> for HNil {
  type Out = Rhs;
  fn concat(self, rhs: Rhs) -> Rhs {
    rhs
  }
}

impl<H: Send + Sync + 'static, T: Concat<Rhs>, Rhs> Concat<Rhs> for HCons<H, T> {
  type Out = HCons<H, T::Out>;
  fn concat(self, rhs: Rhs) -> Self::Out {
    HCons(self.0, self.1.concat(rhs))
  }
}

/// Build an environment value: `hlist![Db::new(), Logger::default()]`.
#[macro_export]
macro_rules! hlist {
  () => { $crate::HNil };
  ($head:expr $(, $rest:expr)* $(,)?) => {
    $crate::HCons::new($head, $crate::hlist!($($rest),*))
  };
}
