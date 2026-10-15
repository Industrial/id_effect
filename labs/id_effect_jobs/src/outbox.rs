//! [`OutboxTable`] trait and in-memory implementation for the transactional outbox pattern.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use id_effect::Effect;
#[cfg(feature = "memory")]
use id_effect::effect;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::OutboxError;

fn now_ms() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
    .unwrap_or(0)
}

/// One outbox row written in the same transaction as domain state (stub: in-memory only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRecord {
  /// Unique row id.
  pub id: String,
  /// Aggregate / stream the event belongs to.
  pub aggregate_id: String,
  /// Event type name for downstream dispatch.
  pub event_type: String,
  /// Serialized payload bytes.
  pub payload: Vec<u8>,
  /// Creation timestamp (epoch ms).
  pub created_ms: i64,
  /// Whether a relay has published this row.
  pub published: bool,
}

impl OutboxRecord {
  /// Build a new unpublished outbox row with a generated id.
  pub fn new(
    aggregate_id: impl Into<String>,
    event_type: impl Into<String>,
    payload: impl Into<Vec<u8>>,
  ) -> Self {
    Self {
      id: Uuid::new_v4().to_string(),
      aggregate_id: aggregate_id.into(),
      event_type: event_type.into(),
      payload: payload.into(),
      created_ms: now_ms(),
      published: false,
    }
  }
}

/// Transactional outbox persistence (append + mark-published).
pub trait OutboxTable: Send + Sync {
  /// Insert a row (typically in the same DB transaction as domain writes).
  fn insert(&self, record: OutboxRecord) -> Effect<OutboxRecord, OutboxError>;

  /// Fetch unpublished rows in creation order, up to `limit`.
  fn fetch_unpublished(&self, limit: usize) -> Effect<Vec<OutboxRecord>, OutboxError>;

  /// Mark rows published after successful relay.
  fn mark_published(&self, ids: &[String]) -> Effect<(), OutboxError>;

  /// Count rows still awaiting relay.
  fn unpublished_count(&self) -> Effect<usize, OutboxError>;
}

/// In-memory outbox table (tests and local demos).
#[cfg(feature = "memory")]
#[derive(Clone)]
pub struct MemoryOutbox {
  rows: Arc<Mutex<BTreeMap<String, OutboxRecord>>>,
}

#[cfg(feature = "memory")]
impl Default for MemoryOutbox {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(feature = "memory")]
impl MemoryOutbox {
  /// Empty table.
  pub fn new() -> Self {
    Self {
      rows: Arc::new(Mutex::new(BTreeMap::new())),
    }
  }
}

#[cfg(feature = "memory")]
impl OutboxTable for MemoryOutbox {
  fn insert(&self, record: OutboxRecord) -> Effect<OutboxRecord, OutboxError> {
    let rows = Arc::clone(&self.rows);
    let stored = record.clone();
    crate::attempt(move || {
      let mut guard = rows.lock().map_err(|e| OutboxError::Lock(e.to_string()))?;
      guard.insert(stored.id.clone(), stored.clone());
      Ok(stored)
    })
  }

  fn fetch_unpublished(&self, limit: usize) -> Effect<Vec<OutboxRecord>, OutboxError> {
    let rows = Arc::clone(&self.rows);
    crate::attempt(move || {
      let guard = rows.lock().map_err(|e| OutboxError::Lock(e.to_string()))?;
      let mut out: Vec<OutboxRecord> = guard.values().filter(|r| !r.published).cloned().collect();
      out.sort_by_key(|r| r.created_ms);
      out.truncate(limit);
      Ok(out)
    })
  }

  fn mark_published(&self, ids: &[String]) -> Effect<(), OutboxError> {
    let ids: Vec<String> = ids.to_vec();
    let rows = Arc::clone(&self.rows);
    crate::attempt(move || {
      let mut guard = rows.lock().map_err(|e| OutboxError::Lock(e.to_string()))?;
      for id in ids {
        let Some(row) = guard.get_mut(&id) else {
          return Err(OutboxError::NotFound(id));
        };
        row.published = true;
      }
      Ok(())
    })
  }

  fn unpublished_count(&self) -> Effect<usize, OutboxError> {
    let rows = Arc::clone(&self.rows);
    crate::attempt(move || {
      let guard = rows.lock().map_err(|e| OutboxError::Lock(e.to_string()))?;
      Ok(guard.values().filter(|r| !r.published).count())
    })
  }
}

/// Relay stub: fetch unpublished rows, invoke `publish`, then mark published.
///
/// Returns the number of rows relayed. Publish failures leave rows unpublished.
#[cfg(feature = "memory")]
pub fn relay_outbox<O, P>(outbox: O, limit: usize, publish: P) -> Effect<usize, OutboxError>
where
  O: OutboxTable + 'static,
  P: Fn(&OutboxRecord) -> Effect<(), OutboxError> + Send + 'static,
{
  effect!({
    let batch = ~outbox.fetch_unpublished(limit);
    let mut published_ids = Vec::new();
    for row in &batch {
      ~publish(row);
      published_ids.push(row.id.clone());
    }
    if !published_ids.is_empty() {
      ~outbox.mark_published(&published_ids);
    }
    published_ids.len()
  })
}

#[cfg(all(test, feature = "memory"))]
mod tests {
  use super::*;
  use id_effect::{fail, succeed};

  fn run<A: Send + 'static>(eff: Effect<A, OutboxError>, _env: ()) -> Result<A, OutboxError> {
    eff
      .block_on()
      .into_result()
      .map_err(|c| c.fail_option().expect("typed failure"))
  }

  #[test]
  fn insert_and_relay_round_trip() {
    let outbox = MemoryOutbox::new();
    let row = OutboxRecord::new("order-1", "OrderPlaced", br#"{"id":1}"#);
    run(outbox.insert(row), ()).unwrap();
    assert_eq!(run(outbox.unpublished_count(), ()).unwrap(), 1);

    let n = run(
      relay_outbox(outbox.clone(), 10, |rec| {
        assert_eq!(rec.event_type, "OrderPlaced");
        succeed::<(), OutboxError, _>(())
      }),
      (),
    )
    .unwrap();
    assert_eq!(n, 1);
    assert_eq!(run(outbox.unpublished_count(), ()).unwrap(), 0);
  }

  #[test]
  fn fetch_respects_limit_and_order() {
    let outbox = MemoryOutbox::new();
    run(outbox.insert(OutboxRecord::new("a", "E1", vec![])), ()).unwrap();
    run(outbox.insert(OutboxRecord::new("b", "E2", vec![])), ()).unwrap();
    let batch = run(outbox.fetch_unpublished(1), ()).unwrap();
    assert_eq!(batch.len(), 1);
  }

  #[test]
  fn mark_published_unknown_id_errors() {
    let outbox = MemoryOutbox::new();
    let err = run(outbox.mark_published(&["missing".into()]), ()).unwrap_err();
    assert_eq!(err, OutboxError::NotFound("missing".into()));
  }

  #[test]
  fn fetch_empty_returns_empty_batch() {
    let outbox = MemoryOutbox::new();
    assert!(run(outbox.fetch_unpublished(5), ()).unwrap().is_empty());
  }

  #[test]
  fn relay_failure_leaves_rows_unpublished() {
    let outbox = MemoryOutbox::new();
    run(outbox.insert(OutboxRecord::new("a", "E", vec![])), ()).unwrap();
    let err = run(
      relay_outbox(outbox.clone(), 5, |_row| {
        fail::<(), OutboxError, _>(OutboxError::Lock("relay fail".into()))
      }),
      (),
    )
    .unwrap_err();
    assert!(matches!(err, OutboxError::Lock(_)));
    assert_eq!(run(outbox.unpublished_count(), ()).unwrap(), 1);
  }
}
