//! [`Tag`] — zero-sized type-level key (`Stratum 3.1`).

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;

/// Pure phantom service identity (`K` is usually a ZST marker type).
#[derive(Clone, Copy)]
pub struct Tag<K: ?Sized>(PhantomData<fn() -> *const K>);

impl<K: ?Sized> fmt::Debug for Tag<K> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Tag")
  }
}

impl<K: ?Sized> Tag<K> {
  /// Canonical [`Tag`] value for the key type `K`.
  pub const fn new() -> Self {
    Self(PhantomData)
  }
}

impl<K: ?Sized> Default for Tag<K> {
  fn default() -> Self {
    Self::new()
  }
}

impl<K: ?Sized> PartialEq for Tag<K> {
  fn eq(&self, _other: &Self) -> bool {
    true
  }
}

impl<K: ?Sized> Eq for Tag<K> {}

impl<K: ?Sized> Hash for Tag<K> {
  fn hash<H: Hasher>(&self, state: &mut H) {
    state.write_u8(0);
  }
}
