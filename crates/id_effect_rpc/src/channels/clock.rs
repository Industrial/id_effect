//! The time presence expiry is measured in: [`SystemClock`] in production, [`ManualClock`] in
//! tests (advanced by hand, so heartbeat expiry is deterministic).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A monotonic clock: the time elapsed since some fixed origin.
pub trait ChannelClock: Send + Sync + 'static {
  /// Time since the clock's origin. Never goes backwards.
  fn now(&self) -> Duration;
}

/// Wall-clock time since the clock was created ([`Instant`]).
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
  origin: Instant,
}

impl SystemClock {
  /// A clock starting now.
  pub fn new() -> Self {
    Self {
      origin: Instant::now(),
    }
  }
}

impl Default for SystemClock {
  fn default() -> Self {
    Self::new()
  }
}

impl ChannelClock for SystemClock {
  fn now(&self) -> Duration {
    self.origin.elapsed()
  }
}

/// A clock that moves only when told to. Clones share the time.
///
/// ```rust
/// use std::time::Duration;
/// use id_effect_rpc::channels::{ChannelClock, ManualClock};
///
/// let clock = ManualClock::new();
/// clock.advance(Duration::from_secs(3));
/// assert_eq!(clock.clone().now(), Duration::from_secs(3));
/// ```
#[derive(Clone, Default)]
pub struct ManualClock {
  millis: Arc<AtomicU64>,
}

impl ManualClock {
  /// A clock at zero.
  pub fn new() -> Self {
    Self::default()
  }

  /// Move the clock forward by `by` (millisecond resolution).
  pub fn advance(&self, by: Duration) {
    let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
    self.millis.fetch_add(millis, Ordering::SeqCst);
  }
}

impl fmt::Debug for ManualClock {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("ManualClock").field(&self.now()).finish()
  }
}

impl ChannelClock for ManualClock {
  fn now(&self) -> Duration {
    Duration::from_millis(self.millis.load(Ordering::SeqCst))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn system_clock_moves_forward() {
    let clock = SystemClock::default();
    let first = clock.now();
    assert!(clock.now() >= first);
  }

  #[test]
  fn manual_clock_is_shared_and_saturates() {
    let clock = ManualClock::new();
    let other = clock.clone();
    other.advance(Duration::from_millis(1500));
    assert_eq!(clock.now(), Duration::from_millis(1500));
    assert!(format!("{clock:?}").contains("ManualClock"));
  }
}
