//! [`Get`] / [`GetMut`] — type-level lookup (`Stratum 3.5`).

use super::hlist::Cons;
use super::path::{Here, There};
use super::tagged::Tagged;

/// Immutable borrow of the value at `Path` for tag `K`.
pub trait Get<K: ?Sized, Path = Here> {
  /// Type of the resolved service at `Path`.
  type Target: ?Sized;
  /// Borrow the value registered for `K` along `Path`.
  fn get(&self) -> &Self::Target;
}

/// Mutable borrow of the value at `Path` for tag `K`.
pub trait GetMut<K: ?Sized, Path = Here> {
  /// Type of the resolved service at `Path`.
  type Target: ?Sized;
  /// Mutably borrow the value registered for `K` along `Path`.
  fn get_mut(&mut self) -> &mut Self::Target;
}

impl<K: ?Sized, V, Tail> Get<K, Here> for Cons<Tagged<K, V>, Tail> {
  type Target = V;
  #[inline]
  fn get(&self) -> &V {
    &self.0.value
  }
}

impl<K: ?Sized, V, Tail> GetMut<K, Here> for Cons<Tagged<K, V>, Tail> {
  type Target = V;
  #[inline]
  fn get_mut(&mut self) -> &mut V {
    &mut self.0.value
  }
}

impl<Head, Tail, K: ?Sized, P> Get<K, There<P>> for Cons<Head, Tail>
where
  Tail: Get<K, P>,
{
  type Target = <Tail as Get<K, P>>::Target;
  #[inline]
  fn get(&self) -> &Self::Target {
    self.1.get()
  }
}

impl<Head, Tail, K: ?Sized, P> GetMut<K, There<P>> for Cons<Head, Tail>
where
  Tail: GetMut<K, P>,
{
  type Target = <Tail as GetMut<K, P>>::Target;
  #[inline]
  fn get_mut(&mut self) -> &mut Self::Target {
    self.1.get_mut()
  }
}
