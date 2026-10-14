//! [`Needs`] — borrow a capability service from the environment.

use super::env::Env;
use super::key::Cap;
use std::ops::Deref;

/// Environment `R` provides service `T`.
///
/// Lookups are `#[track_caller]`: when `T` is absent at runtime the panic names the service and
/// the `~T` / `Needs::<T>::need(r)` call site.
#[diagnostic::on_unimplemented(
  message = "environment `{Self}` cannot provide capability `{T}`",
  label = "`{T}` is looked up here, but `{Self}` is not a capability environment",
  note = "run the effect in a capability environment: `Effect<_, _, caps!({T})>` (or `Env`), not `()`",
  note = "a service type must be `Clone + Send + Sync + 'static`"
)]
pub trait Needs<T>
where
  T: Clone + Send + Sync + 'static,
{
  /// Borrow the service registered for `T`.
  ///
  /// # Panics
  ///
  /// When `T` is not in the environment (message includes the caller's `file:line`).
  fn need(&self) -> &T;
}

impl<T> Needs<T> for Env
where
  T: Clone + Send + Sync + 'static,
{
  #[inline]
  #[track_caller]
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
  #[track_caller]
  fn need(&self) -> &T {
    self.deref().get::<Cap<T>>()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::CapList;

  #[derive(Clone, Debug, PartialEq)]
  struct Greeting(&'static str);

  #[test]
  fn need_reads_service_from_env_and_cap_list() {
    let mut env = Env::new();
    env.insert::<Cap<Greeting>>(Greeting("hi"));
    assert_eq!(Needs::<Greeting>::need(&env), &Greeting("hi"));
    let caps = CapList::<(Cap<Greeting>,)>::new(env);
    assert_eq!(Needs::<Greeting>::need(&caps), &Greeting("hi"));
  }

  #[test]
  fn need_missing_panics_at_the_call_site() {
    let caps = CapList::<(Cap<Greeting>,)>::new(Env::new());
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let _ = Needs::<Greeting>::need(&caps);
    }))
    .expect_err("missing capability must panic");
    let line = line!() - 3;
    let msg = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("Greeting"), "{msg}");
    assert!(msg.contains(&format!("{}:{line}", file!())), "{msg}");
  }
}
