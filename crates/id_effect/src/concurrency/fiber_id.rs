//! [`FiberId`] — branded `u64` identity for a fiber (§ 7.1).
//!
//! Standalone primitive: no stratum dependency beyond `std::sync::atomic`.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

/// Branded `u64` identifier for a fiber; [`Display`] formats as `fiber-{id}`.
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FiberId(pub u64);

impl Copy for FiberId {}

impl FiberId {
  /// Create a new id from a raw `u64` (prefer [`Self::fresh`] for new fibers).
  #[inline]
  pub fn new(v: u64) -> Self {
    Self(v)
  }

  /// Unwrap to the inner `u64`.
  #[inline]
  pub fn into_inner(self) -> u64 {
    self.0
  }

  /// Root fiber id (`0`), used as the default parent in some APIs.
  pub const ROOT: Self = Self(0);

  /// Allocate a new unique id (monotonic, relaxed ordering).
  #[inline]
  pub fn fresh() -> Self {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
  }

  /// Raw numeric id.
  #[inline]
  pub fn as_u64(self) -> u64 {
    self.0
  }

  /// `true` if this is [`Self::ROOT`].
  #[inline]
  pub fn is_root(self) -> bool {
    self == Self::ROOT
  }
}

impl fmt::Display for FiberId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "fiber-{}", self.0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod fiber_id {
    use super::*;

    #[test]
    fn root_and_fresh_ids_when_formatted_follow_root_contract() {
      let root = FiberId::ROOT;
      assert!(root.is_root());
      assert_eq!(root.to_string(), "fiber-0");

      let fresh = FiberId::fresh();
      assert!(!fresh.is_root());
      assert!(fresh.to_string().starts_with("fiber-"));
    }

    #[test]
    fn fiber_id_brand_derives_debug() {
      let id = FiberId::ROOT;
      assert_eq!(format!("{id:?}"), "FiberId(0)");
    }

    #[test]
    fn fiber_id_orders_by_inner_u64() {
      let a = FiberId::new(1);
      let b = FiberId::new(2);
      assert!(a < b);
    }

    #[test]
    fn fresh_ids_are_strictly_increasing() {
      let ids: Vec<_> = (0..10).map(|_| FiberId::fresh()).collect();
      for w in ids.windows(2) {
        assert!(w[0] < w[1]);
      }
    }

    #[test]
    fn into_inner_round_trips() {
      let id = FiberId::new(42);
      assert_eq!(id.into_inner(), 42);
      assert_eq!(id.as_u64(), 42);
    }
  }
}
