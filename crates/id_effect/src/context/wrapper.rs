//! [`Context`] newtype and composition helpers (`Stratum 3.6` / `3.8`).

use super::get::{Get, GetMut};
use super::hlist::Cons;
use super::path::Here;

/// Newtype around an HList used as the interpreter environment for [`crate::kernel::Effect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Context<L>(pub L);

impl<L> AsRef<L> for Context<L> {
  #[inline]
  fn as_ref(&self) -> &L {
    &self.0
  }
}

impl<L> AsMut<L> for Context<L> {
  #[inline]
  fn as_mut(&mut self) -> &mut L {
    &mut self.0
  }
}

impl<L> Context<L> {
  /// Wrap heterogeneous service list `list` as an effect environment.
  #[inline]
  pub const fn new(list: L) -> Self {
    Self(list)
  }

  /// Unwrap to the inner HList.
  #[inline]
  pub fn into_inner(self) -> L {
    self.0
  }

  /// Resolve tag `K` at path [`Here`] (this cell must be [`Tagged`](super::tagged::Tagged)).
  #[inline]
  pub fn get<K>(&self) -> &<L as Get<K, Here>>::Target
  where
    L: Get<K, Here>,
  {
    Get::<K, Here>::get(&self.0)
  }

  /// Like `get` with an explicit path (e.g. `Skip1` / [`super::path::ThereHere`] for the second cell).
  #[inline]
  pub fn get_path<K, P>(&self) -> &<L as Get<K, P>>::Target
  where
    L: Get<K, P>,
  {
    Get::<K, P>::get(&self.0)
  }

  /// Mutable head-cell access (same path rules as `get`).
  #[inline]
  pub fn get_mut<K>(&mut self) -> &mut <L as GetMut<K, Here>>::Target
  where
    L: GetMut<K, Here>,
  {
    GetMut::<K, Here>::get_mut(&mut self.0)
  }

  /// Mutable `get_path`.
  #[inline]
  pub fn get_mut_path<K, P>(&mut self) -> &mut <L as GetMut<K, P>>::Target
  where
    L: GetMut<K, P>,
  {
    GetMut::<K, P>::get_mut(&mut self.0)
  }

  /// Prepend a new head row: `SPEC` `prepend: H → Context[L] → Context[Cons[H, L]]`.
  #[inline]
  pub fn prepend<H>(self, head: H) -> Context<Cons<H, L>> {
    prepend_cell(head, self)
  }
}

impl<H, T> Context<Cons<H, T>> {
  /// Project the head of the inner [`Cons`].
  #[inline]
  pub fn head(&self) -> &H {
    &self.0.0
  }

  /// Borrow the tail of the inner [`Cons`] (the remainder of the HList).
  #[inline]
  pub fn tail_list(&self) -> &T {
    &self.0.1
  }

  /// Drop the head cell and wrap only the tail as a [`Context`].
  #[inline]
  pub fn into_tail(self) -> Context<T> {
    Context::new(self.0.1)
  }
}

/// Free function form of [`Context::prepend`].
#[inline]
pub fn prepend_cell<H, L>(head: H, ctx: Context<L>) -> Context<Cons<H, L>> {
  Context::new(Cons(head, ctx.into_inner()))
}

impl<K: ?Sized, P, L> Get<K, P> for Context<L>
where
  L: Get<K, P>,
{
  type Target = <L as Get<K, P>>::Target;
  #[inline]
  fn get(&self) -> &Self::Target {
    self.0.get()
  }
}

impl<K: ?Sized, P, L> GetMut<K, P> for Context<L>
where
  L: GetMut<K, P>,
{
  type Target = <L as GetMut<K, P>>::Target;
  #[inline]
  fn get_mut(&mut self) -> &mut Self::Target {
    self.0.get_mut()
  }
}
