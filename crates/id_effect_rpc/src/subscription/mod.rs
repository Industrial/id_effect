//! Sources for [`Subscription`](crate::Subscription) procedures (design note §6).
//!
//! A subscription is a resumable stream: every item carries the [`Cursor`] a client resumes
//! after. This module holds what every source shares — the bounded tail configuration
//! ([`TailConfig`]) and the cursor check ([`position_after`], [`CursorMismatch`]) — and, behind
//! feature `events`, the `events` module: a subscription over an `id_effect_events` `EventLog`
//! whose cursor is the log's global position (`Cursor::events(pos)`).
//!
//! Rules every source follows:
//!
//! - **A cursor is not a capability.** It names a position, not a right. Resuming is a new call
//!   with a cursor, and every call goes through [`dispatch::open`](crate::server::dispatch::open),
//!   so the procedure's guards run again on every subscribe and on every resume.
//! - **Replay, then tail, from one position.** A source reads from the cursor and keeps reading
//!   from the last position it delivered, so there is no hand-over between "history" and "live"
//!   where an item could be lost or repeated.
//! - **Backpressure is pull-based (design note §5).** A source reads only when the transport
//!   pulls; a slow subscriber leaves its items in the log rather than in memory. What one call
//!   holds is bounded by the stream window plus one read batch ([`TailConfig::batch_size`]); a
//!   subscriber that falls behind is simply *lagged* and catches up from its own position.
//!
//! ```rust
//! use std::time::Duration;
//! use id_effect_rpc::subscription::{TailConfig, position_after};
//! use id_effect_rpc::wire::{Cursor, CursorSource};
//!
//! let config = TailConfig::new()
//!   .with_batch_size(50)
//!   .with_poll_interval(Duration::from_millis(100));
//! assert_eq!(config.batch_size(), 50);
//! // Out-of-range values are clamped to the bounds.
//! assert_eq!(TailConfig::new().with_batch_size(1_000_000).batch_size(), 1_000);
//!
//! assert_eq!(position_after(Some(Cursor::events(7)), CursorSource::Events), Ok(Some(7)));
//! assert!(position_after(Some(Cursor::sequence(7)), CursorSource::Events).is_err());
//! ```

use std::time::Duration;

use http::StatusCode;
use serde_json::{Value, json};

use crate::error_union::{RpcErrorStatus, RpcErrorUnion};
use crate::wire::{Cursor, CursorSource};

#[cfg(feature = "events")]
pub mod events;

/// Default number of entries one read of the source returns.
pub const DEFAULT_BATCH_SIZE: usize = 100;

/// The largest read batch [`TailConfig::with_batch_size`] accepts.
pub const MAX_BATCH_SIZE: usize = 1_000;

/// Default wait between two reads that found nothing new.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The shortest poll interval [`TailConfig::with_poll_interval`] accepts.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The longest poll interval [`TailConfig::with_poll_interval`] accepts.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How a subscription source reads: entries per read and the wait after a read that found
/// nothing new. Both are clamped to fixed bounds, so no configuration makes one call read an
/// unbounded batch or poll in a busy loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailConfig {
  batch_size: usize,
  poll_interval: Duration,
}

impl Default for TailConfig {
  fn default() -> Self {
    Self {
      batch_size: DEFAULT_BATCH_SIZE,
      poll_interval: DEFAULT_POLL_INTERVAL,
    }
  }
}

impl TailConfig {
  /// [`DEFAULT_BATCH_SIZE`] entries per read, [`DEFAULT_POLL_INTERVAL`] between empty reads.
  pub fn new() -> Self {
    Self::default()
  }

  /// Read `batch_size` entries at a time (clamped to `1..=`[`MAX_BATCH_SIZE`]).
  #[must_use]
  pub fn with_batch_size(mut self, batch_size: usize) -> Self {
    self.batch_size = batch_size.clamp(1, MAX_BATCH_SIZE);
    self
  }

  /// Wait `interval` after a read that found nothing new (clamped to
  /// [`MIN_POLL_INTERVAL`]`..=`[`MAX_POLL_INTERVAL`]).
  #[must_use]
  pub fn with_poll_interval(mut self, interval: Duration) -> Self {
    self.poll_interval = interval.clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL);
    self
  }

  /// Entries per read.
  pub fn batch_size(&self) -> usize {
    self.batch_size
  }

  /// The wait after an empty read.
  pub fn poll_interval(&self) -> Duration {
    self.poll_interval
  }
}

/// A subscription was asked to resume from a cursor of another source (a per-session sequence
/// cursor on an event-log subscription, say). On the wire: `{"_tag": "CursorMismatch"}`, 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {found:?} cursor cannot resume a subscription over {expected:?} positions")]
pub struct CursorMismatch {
  /// The source the subscription counts.
  pub expected: CursorSource,
  /// The source the cursor named.
  pub found: CursorSource,
}

impl RpcErrorStatus for CursorMismatch {
  fn status(&self) -> StatusCode {
    StatusCode::BAD_REQUEST
  }
}

/// `{"_tag": "CursorMismatch"}`: the cursor's source is not echoed (cursors are opaque).
impl RpcErrorUnion for CursorMismatch {
  fn to_wire(&self) -> Value {
    json!({"_tag": "CursorMismatch"})
  }

  fn union_schema() -> Option<Value> {
    Some(json!({
      "oneOf": [{
        "type": "object",
        "properties": {"_tag": {"const": "CursorMismatch"}},
        "required": ["_tag"]
      }]
    }))
  }
}

/// The position to resume after: `Ok(None)` without a cursor, the cursor's position when it
/// counts `expected`, [`CursorMismatch`] otherwise.
pub fn position_after(
  after: Option<Cursor>,
  expected: CursorSource,
) -> Result<Option<u64>, CursorMismatch> {
  match after {
    None => Ok(None),
    Some(cursor) if cursor.source == expected => Ok(Some(cursor.position)),
    Some(cursor) => Err(CursorMismatch {
      expected,
      found: cursor.source,
    }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tail_config_is_clamped() {
    let config = TailConfig::new()
      .with_batch_size(0)
      .with_poll_interval(Duration::ZERO);
    assert_eq!(config.batch_size(), 1);
    assert_eq!(config.poll_interval(), MIN_POLL_INTERVAL);
    let config = TailConfig::new().with_poll_interval(Duration::from_secs(3600));
    assert_eq!(config.poll_interval(), MAX_POLL_INTERVAL);
    assert_eq!(TailConfig::default().batch_size(), DEFAULT_BATCH_SIZE);
    assert_eq!(TailConfig::default().poll_interval(), DEFAULT_POLL_INTERVAL);
  }

  #[test]
  fn cursor_mismatch_is_a_tagged_400() {
    assert_eq!(
      position_after(None, CursorSource::Sequence),
      Ok(None),
      "no cursor: the source's own start"
    );
    let err = position_after(Some(Cursor::events(3)), CursorSource::Sequence).unwrap_err();
    assert_eq!(err.found, CursorSource::Events);
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err.to_wire(), json!({"_tag": "CursorMismatch"}));
    assert!(!err.is_internal());
    assert!(CursorMismatch::union_schema().is_some());
    assert!(err.to_string().contains("Sequence"));
  }
}
