//! Transactional outbox backed by [obix](https://docs.rs/obix) on a shared [`PgPool`](sqlx::PgPool).
//!
//! Implements [`OutboxTable`] by persisting [`JobsOutboxEvent`] rows through obix and tracking
//! relay progress with a monotonic sequence cursor (obix has no `published` flag).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use id_effect::Effect;
use obix::{EventSequence, MailboxConfig, Outbox};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::error::OutboxError;
use crate::outbox::{OutboxRecord, OutboxTable};

/// Event payload stored in obix `persistent_outbox_events.payload`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsOutboxEvent {
  /// Row id used by [`OutboxRecord`].
  pub id: String,
  /// Aggregate / stream id.
  pub aggregate_id: String,
  /// Event type for downstream dispatch.
  pub event_type: String,
  /// Serialized payload bytes.
  pub payload: Vec<u8>,
  /// Creation timestamp (epoch ms).
  pub created_ms: i64,
}

impl JobsOutboxEvent {
  fn from_record(record: &OutboxRecord) -> Self {
    Self {
      id: record.id.clone(),
      aggregate_id: record.aggregate_id.clone(),
      event_type: record.event_type.clone(),
      payload: record.payload.clone(),
      created_ms: record.created_ms,
    }
  }

  fn into_record(self, published: bool) -> OutboxRecord {
    OutboxRecord {
      id: self.id,
      aggregate_id: self.aggregate_id,
      event_type: self.event_type,
      payload: self.payload,
      created_ms: self.created_ms,
      published,
    }
  }
}

/// Production outbox using obix on PostgreSQL.
#[derive(Clone)]
pub struct ObixOutbox {
  inner: Outbox<JobsOutboxEvent>,
  pool: PgPool,
  relay_cursor: Arc<Mutex<EventSequence>>,
  last_fetched: Arc<Mutex<HashMap<String, EventSequence>>>,
}

impl ObixOutbox {
  /// Initialize obix listeners and caches on `pool`.
  pub async fn init(pool: &PgPool, config: MailboxConfig) -> Result<Self, OutboxError> {
    let inner = Outbox::<JobsOutboxEvent>::init(pool, config)
      .await
      .map_err(|e| OutboxError::Storage(e.to_string()))?;
    Ok(Self {
      inner,
      pool: pool.clone(),
      relay_cursor: Arc::new(Mutex::new(EventSequence::BEGIN)),
      last_fetched: Arc::new(Mutex::new(HashMap::new())),
    })
  }

  /// Underlying obix outbox (register handlers, listen for NOTIFY, etc.).
  pub fn obix(&self) -> &Outbox<JobsOutboxEvent> {
    &self.inner
  }

  /// Shared PostgreSQL pool.
  pub fn pool(&self) -> &PgPool {
    &self.pool
  }
}

impl OutboxTable for ObixOutbox {
  fn insert(&self, record: OutboxRecord) -> Effect<OutboxRecord, OutboxError, ()> {
    let inner = self.inner.clone();
    let stored = record.clone();
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let event = JobsOutboxEvent::from_record(&stored);
        let mut op = inner
          .begin_op()
          .await
          .map_err(|e| OutboxError::Storage(e.to_string()))?;
        inner
          .publish_persisted_in_op(&mut op, event)
          .await
          .map_err(|e| OutboxError::Storage(e.to_string()))?;
        op.commit()
          .await
          .map_err(|e| OutboxError::Storage(e.to_string()))?;
        Ok(stored)
      })
    })
  }

  fn fetch_unpublished(&self, limit: usize) -> Effect<Vec<OutboxRecord>, OutboxError, ()> {
    let pool = self.pool.clone();
    let relay_cursor = Arc::clone(&self.relay_cursor);
    let last_fetched = Arc::clone(&self.last_fetched);
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let cursor = *relay_cursor
          .lock()
          .map_err(|e| OutboxError::Lock(e.to_string()))?;
        let rows: Vec<(i64, Value)> = sqlx::query_as(
          "SELECT sequence, payload FROM persistent_outbox_events            WHERE sequence > $1 ORDER BY sequence ASC LIMIT $2",
        )
        .bind(u64::from(cursor) as i64)
        .bind(limit as i64)
        .fetch_all(&pool)
        .await
        .map_err(|e| OutboxError::Storage(e.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        let mut cache = last_fetched
          .lock()
          .map_err(|e| OutboxError::Lock(e.to_string()))?;
        cache.clear();
        for (sequence, payload) in rows {
          let event: JobsOutboxEvent =
            serde_json::from_value(payload).map_err(|e| OutboxError::Storage(e.to_string()))?;
          cache.insert(event.id.clone(), EventSequence::from(sequence as u64));
          out.push(event.into_record(false));
        }
        Ok(out)
      })
    })
  }

  fn mark_published(&self, ids: &[String]) -> Effect<(), OutboxError, ()> {
    let ids: Vec<String> = ids.to_vec();
    let relay_cursor = Arc::clone(&self.relay_cursor);
    let last_fetched = Arc::clone(&self.last_fetched);
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let cache = last_fetched
          .lock()
          .map_err(|e| OutboxError::Lock(e.to_string()))?;
        let mut cursor = relay_cursor
          .lock()
          .map_err(|e| OutboxError::Lock(e.to_string()))?;
        let mut advanced = false;
        for id in &ids {
          let Some(sequence) = cache.get(id) else {
            return Err(OutboxError::NotFound(id.clone()));
          };
          if u64::from(*sequence) > u64::from(*cursor) {
            *cursor = *sequence;
            advanced = true;
          }
        }
        if !advanced && !ids.is_empty() {
          // ids were present but cursor already past them — ok
        }
        Ok(())
      })
    })
  }

  fn unpublished_count(&self) -> Effect<usize, OutboxError, ()> {
    let pool = self.pool.clone();
    let relay_cursor = Arc::clone(&self.relay_cursor);
    Effect::new_async(move |_r| {
      Box::pin(async move {
        let cursor = *relay_cursor
          .lock()
          .map_err(|e| OutboxError::Lock(e.to_string()))?;
        let row: (i64,) =
          sqlx::query_as("SELECT COUNT(*) FROM persistent_outbox_events WHERE sequence > $1")
            .bind(u64::from(cursor) as i64)
            .fetch_one(&pool)
            .await
            .map_err(|e| OutboxError::Storage(e.to_string()))?;
        Ok(row.0.max(0) as usize)
      })
    })
  }
}
