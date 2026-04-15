//! [`Tagged`] key–value cell (`Stratum 3.2`).

use super::tag::Tag;

/// Associates runtime value `V` with key type `K` inside a [`Cons`](super::hlist::Cons) list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tagged<K: ?Sized, V> {
  pub(crate) _tag: Tag<K>,
  /// Runtime value stored under the phantom key `K`.
  pub value: V,
}

impl<K: ?Sized, V> Tagged<K, V> {
  /// Pair `value` with the key type `K` via [`Tag::new`].
  #[inline]
  pub const fn new(value: V) -> Self {
    Self {
      _tag: Tag::new(),
      value,
    }
  }
}

/// SPEC `tagged[K, V]: V → Tagged[K, V]` — alias of [`Tagged::new`].
#[inline]
pub const fn tagged<K: ?Sized, V>(value: V) -> Tagged<K, V> {
  Tagged::new(value)
}
