//! [`Needs`] — borrow a capability service from the environment.

use super::env::Env;
use super::key::Cap;
use std::ops::Deref;

/// Environment `R` provides service `T`.
pub trait Needs<T>
where
  T: Clone + Send + Sync + 'static,
{
  /// Borrow the service registered for `T`.
  fn need(&self) -> &T;
}

impl<T> Needs<T> for Env
where
  T: Clone + Send + Sync + 'static,
{
  #[inline]
  fn need(&self) -> &T {
    self.get::<Cap<T>>()
  }
}

impl<T, R> Needs<T> for R
where
  T: Clone + Send + Sync + 'static,
  R: Deref<Target = Env>,
{
  #[inline]
  fn need(&self) -> &T {
    self.deref().get::<Cap<T>>()
  }
}
