//! Typed environments: `HNil`/`HCons`/`Here`/`There`, `HList` and the
//! `Selector`/`Plucker`/`Sculptor`/`Embed`/`Concat` traits with their diagnostics.
//!
//! The environment `R` of an `Effect` is an HList of `Arc`ed capabilities. Lookups, provision,
//! subset projection and concatenation are traits whose index parameters the compiler infers
//! (design A §2.1). A missing capability is a compile error with a custom diagnostic.
//!
//! # Diagnostics
//!
//! `Selector`, `Plucker`, `Sculptor` and `Embed` carry `#[diagnostic::on_unimplemented]`, so a
//! missing, superfluous or non-embeddable capability is reported in capability terms. Every
//! recursive impl (`There<I>` for the single-element traits, `HCons<I, Is>` for the list traits)
//! is `#[diagnostic::do_not_recommend]`: rustc then reports the failed obligation at the trait the
//! user's code named, with one `help` line, instead of a `required for ..` chain that grows with
//! the length of `R`. The `.stderr` shapes are pinned by `k-ui-suite`.
//!
//! A capability that appears twice in `R` is ambiguous (E0283, "type annotations needed"):
//! `on_unimplemented` does not fire on ambiguity. `caps!` rejects duplicates at the
//! declaration, and `Layer::merge` deduplicates shared inputs (`k-layer-base`).

use std::marker::PhantomData;
use std::sync::Arc;

/// The empty environment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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
  /// `head` (wrapped in an `Arc`) in front of `tail`.
  pub fn new(head: H, tail: T) -> Self {
    Self(Arc::new(head), tail)
  }
}

/// Index witness: the capability is the head.
#[derive(Debug)]
pub struct Here;

/// Index witness: the capability is somewhere in the tail.
#[derive(Debug)]
pub struct There<I>(PhantomData<I>);

/// An environment value: cheap to clone (one `Arc` bump per capability), shareable across fibers.
pub trait HList: Clone + Send + Sync + 'static {
  /// Number of capabilities.
  const LEN: usize;
}

impl HList for HNil {
  const LEN: usize = 0;
}

impl<H: Send + Sync + 'static, T: HList> HList for HCons<H, T> {
  const LEN: usize = 1 + T::LEN;
}

/// Read capability `T` at the inferred index `I`. A missing `T` is a compile error.
#[diagnostic::on_unimplemented(
  message = "capability `{T}` is not in this effect's environment",
  label = "`{T}` is required here",
  note = "add `{T}` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running"
)]
pub trait Selector<T, I> {
  /// The capability.
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
  /// The environment without `T`.
  type Rest: HList;
  /// Put `t` back at its index in `rest`.
  fn plant(t: Arc<T>, rest: Self::Rest) -> Self;
  /// Take `T` out, returning it and the rest.
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
  label = "cannot provide `{Sub}` here",
  note = "every capability given to `.provide_some(..)` must be in the effect's `R`; provide the others separately or drop them"
)]
pub trait Sculptor<Sub, Is>: Sized {
  /// The environment without `Sub`.
  type Rest: HList;
  /// Put every capability of `sub` back at its index in `rest`.
  fn plant(sub: Sub, rest: Self::Rest) -> Self;
}

impl<S: HList> Sculptor<HNil, HNil> for S {
  type Rest = S;
  fn plant(_: HNil, rest: S) -> S {
    rest
  }
}

#[diagnostic::do_not_recommend]
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

/// Read-only projection of a sub-environment `Sub` out of `Self` (any subset, any order): what
/// narrowing (`~sub_effect`, `.narrow()`, `.provide_env(..)`) uses. Costs one `Arc` clone per
/// capability in `Sub`.
#[diagnostic::on_unimplemented(
  message = "the environment `{Self}` does not contain every capability of `{Sub}`",
  label = "cannot narrow `{Self}` to `{Sub}`",
  note = "every capability of `{Sub}` must be in `{Self}`; add the missing ones to the caller's `caps!(..)` (for a `~` bind, a `~(..)` branch or `.narrow()`), to the `hlist![..]` (for `.provide_env(..)`) or to the layer's output (for `.provide_layer(..)` / `.provide_to(..)`)"
)]
pub trait Embed<Sub, Is> {
  /// The sub-environment.
  fn project(&self) -> Sub;
}

impl<S> Embed<HNil, HNil> for S {
  fn project(&self) -> HNil {
    HNil
  }
}

#[diagnostic::do_not_recommend]
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
  /// `Self` followed by `Rhs`.
  type Out: HList;
  /// Append `rhs`.
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

/// Builds an environment value: `hlist![Db::new(), Logger::default()]`.
#[doc(hidden)]
#[macro_export]
macro_rules! __hlist {
  () => { $crate::HNil };
  ($head:expr $(, $rest:expr)* $(,)?) => {
    $crate::HCons::new($head, $crate::__hlist!($($rest),*))
  };
}

#[doc(inline)]
pub use crate::__hlist as hlist;

#[cfg(test)]
mod tests {
  use super::*;

  struct A(u8);
  struct B(&'static str);
  struct C;

  fn get<T, I, L: Selector<T, I>>(l: &L) -> &Arc<T> {
    l.get()
  }

  #[test]
  fn select_pluck_sculpt_embed_concat() {
    let env = hlist![A(1), B("b"), C];
    assert_eq!(<HCons<A, HCons<B, HCons<C, HNil>>> as HList>::LEN, 3);
    assert_eq!(get::<A, _, _>(&env).0, 1);
    assert_eq!(get::<B, _, _>(&env).0, "b");

    let (b, rest): (Arc<B>, HCons<A, HCons<C, HNil>>) = Plucker::<B, _>::pluck(env.clone());
    assert_eq!(b.0, "b");
    let back: HCons<A, HCons<B, HCons<C, HNil>>> = Plucker::<B, _>::plant(b, rest);
    assert_eq!(get::<A, _, _>(&back).0, 1);

    let sub: HCons<C, HCons<A, HNil>> = Embed::<_, _>::project(&env);
    assert_eq!(get::<A, _, _>(&sub).0, 1);

    let rest = HCons::new(B("x"), HNil);
    let whole: HCons<A, HCons<B, HCons<C, HNil>>> =
      Sculptor::<HCons<A, HCons<C, HNil>>, _>::plant(hlist![A(3), C], rest);
    assert_eq!(get::<B, _, _>(&whole).0, "x");
    assert_eq!(get::<A, _, _>(&whole).0, 3);

    let cat = hlist![A(2)].concat(hlist![C]);
    let _: &HCons<A, HCons<C, HNil>> = &cat;
  }
}
