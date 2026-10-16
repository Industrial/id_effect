//! Fallback policy — answer with an alternative when an effect fails.
//!
//! A [`Fallback`] is a reusable recovery: [`Fallback::apply`] runs an effect and, on a typed
//! failure the policy [handles](Fallback::when), runs the fallback effect instead (a cached
//! value, a degraded answer, another source). Failures it does not handle, defects and
//! interrupts pass through unchanged, as with [`Effect::catch`].

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, fail, succeed, suspend};

type Recover<A, E, R> = Arc<dyn Fn(E) -> Effect<A, E, R> + Send + Sync>;
type Handles<E> = Arc<dyn Fn(&E) -> bool + Send + Sync>;

/// A reusable fallback for effects of type `Effect<A, E, R>`. Cloning is cheap.
///
/// ```
/// use id_effect::{Effect, Exit, fail};
/// use id_effect_resilience::Fallback;
///
/// let cached = Fallback::value("cached profile").when(|e: &&str| *e == "timeout");
/// let slow: Effect<&str, &str> = fail("timeout");
/// assert_eq!(cached.apply(slow).block_on(), Exit::Success("cached profile"));
/// // Not handled: the failure passes through.
/// let denied: Effect<&str, &str> = fail("forbidden");
/// assert_eq!(cached.apply(denied).block_on(), Exit::fail("forbidden"));
/// ```
pub struct Fallback<A, E, R = id_effect::HNil> {
  handles: Handles<E>,
  recover: Recover<A, E, R>,
}

impl<A, E, R> Clone for Fallback<A, E, R> {
  fn clone(&self) -> Self {
    Self {
      handles: Arc::clone(&self.handles),
      recover: Arc::clone(&self.recover),
    }
  }
}

impl<A, E, R> fmt::Debug for Fallback<A, E, R> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Fallback(..)")
  }
}

impl<A, E, R> Fallback<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  /// Recovers from every typed failure `e` with `recover(e)` (which may itself fail).
  pub fn new(recover: impl Fn(E) -> Effect<A, E, R> + Send + Sync + 'static) -> Self {
    Self {
      handles: Arc::new(|_| true),
      recover: Arc::new(recover),
    }
  }

  /// Recovers from every typed failure with a copy of `value`.
  pub fn value(value: A) -> Self
  where
    A: Clone + Sync,
  {
    Self::new(move |_| succeed(value.clone()))
  }

  /// Restricts this fallback to the failures for which `pred` holds.
  pub fn when(mut self, pred: impl Fn(&E) -> bool + Send + Sync + 'static) -> Self {
    self.handles = Arc::new(pred);
    self
  }

  /// `true` when this fallback handles `error`.
  pub fn handles(&self, error: &E) -> bool {
    (self.handles)(error)
  }

  /// Runs `effect`, recovering from a handled typed failure with the fallback.
  pub fn apply(&self, effect: Effect<A, E, R>) -> Effect<A, E, R> {
    let this = self.clone();
    effect.catch(move |e| {
      if this.handles(&e) {
        suspend(move || (this.recover)(e))
      } else {
        fail(e)
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit, sync};

  use super::*;
  use crate::test_support::run_ok;

  #[test]
  fn recovers_from_handled_failures_only() {
    let fb = Fallback::new(|e: u32| succeed(e * 10)).when(|e| *e < 5);
    assert_eq!(run_ok(fb.apply(fail(3))), Ok(30));
    assert_eq!(run_ok(fb.apply(fail(7))), Err(7));
    assert_eq!(run_ok(fb.apply(succeed(1))), Ok(1));
    assert!(fb.handles(&4) && !fb.handles(&5));
  }

  #[test]
  fn fallback_may_fail_itself() {
    let fb: Fallback<u32, &str> = Fallback::new(|_| fail("fallback down"));
    assert_eq!(run_ok(fb.apply(fail("primary down"))), Err("fallback down"));
  }

  #[test]
  fn defects_pass_through() {
    let fb: Fallback<u32, &str> = Fallback::value(0);
    let boom: Effect<u32, &str> = sync(|| panic!("bug"));
    assert!(matches!(
      fb.apply(boom).block_on(),
      Exit::Failure(Cause::Die(_))
    ));
    assert_eq!(format!("{fb:?}"), "Fallback(..)");
  }
}
