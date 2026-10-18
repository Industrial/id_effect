//! Instance leases and fencing (release 0.11.0 decision D7): [`EngineId`], [`Lease`],
//! [`Fenced`].
//!
//! An engine advances an instance only while it holds the instance's lease. Taking a lease
//! (a free one, or one whose `lease_expires_at` has passed) increments the instance's
//! `fencing_token`; every later write of that engine (history append, lease renewal, finishing
//! the instance) names the token it got and is applied only while the row still carries it and,
//! for appends and finishing, while the lease has not expired. A writer that paused past its
//! lease (GC, a stalled node, a partition) therefore cannot append once another engine has taken
//! the instance over, nor after its own lease ran out.

use std::fmt;
use std::time::Duration;

use id_effect::{Random, UtcDateTime};

use crate::workflow::WorkflowId;

/// The identity an engine takes leases under (the `lease_owner` column). Unique per running
/// engine: [`EngineId::random`] by default.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EngineId(String);

impl EngineId {
  /// The engine id `id`.
  pub fn new(id: impl Into<String>) -> Self {
    Self(id.into())
  }

  /// `engine-<16 hex digits>` from OS entropy.
  pub fn random() -> Self {
    Self(format!("engine-{:016x}", Random::live().next_u64()))
  }

  /// The id as a string slice.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for EngineId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.pad(&self.0)
  }
}

/// A held lease on one instance: who holds it, with which fencing token, until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
  /// The instance.
  pub instance: WorkflowId,
  /// The holder.
  pub owner: EngineId,
  /// The fencing token the acquisition produced; every fenced write names it.
  pub token: i64,
  /// When the lease runs out unless renewed.
  pub expires_at: UtcDateTime,
}

/// The result of a fenced write: applied, or rejected because the writer no longer holds the
/// lease (another token, another owner, an expired lease or a finished instance).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Fenced {
  /// The write was applied.
  Applied,
  /// The write was rejected; the writer must stop advancing the instance.
  Lost,
}

impl Fenced {
  /// `Applied` when `rows` rows were written, else `Lost`.
  pub(crate) fn from_rows(rows: u64) -> Self {
    if rows == 0 {
      Fenced::Lost
    } else {
      Fenced::Applied
    }
  }

  /// Whether the write was applied.
  pub fn is_applied(self) -> bool {
    self == Fenced::Applied
  }
}

/// `at + d`, saturating at the end of time.
pub(crate) fn later(at: UtcDateTime, d: Duration) -> UtcDateTime {
  let d = jiff::SignedDuration::try_from(d).unwrap_or(jiff::SignedDuration::MAX);
  UtcDateTime(at.0.checked_add(d).unwrap_or(jiff::Timestamp::MAX))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fenced_from_rows_and_later_saturates() {
    assert_eq!(Fenced::from_rows(0), Fenced::Lost);
    assert!(Fenced::from_rows(1).is_applied());
    let t = UtcDateTime::unsafe_make(1_000);
    assert_eq!(
      later(t, Duration::from_secs(2)),
      UtcDateTime::unsafe_make(3_000)
    );
    assert_eq!(later(t, Duration::MAX).0, jiff::Timestamp::MAX);
  }

  #[test]
  fn random_engine_ids_differ() {
    let (a, b) = (EngineId::random(), EngineId::random());
    assert_ne!(a, b);
    assert!(a.as_str().starts_with("engine-") && a.as_str().len() == 23);
    assert_eq!(EngineId::new("a").to_string(), "a");
  }
}
