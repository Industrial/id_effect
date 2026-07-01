//! Transactional outbox backed by [obix](https://docs.rs/obix) on a shared [`PgPool`](sqlx::PgPool).

use id_effect::Effect;
use obix::{MailboxConfig, Outbox, OutboxEventHandler, OutboxEventJobConfig};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::error::OutboxError;
use crate::outbox::OutboxRecord;

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
}

/// Production outbox using obix on PostgreSQL.
#[derive(Clone)]
pub struct ObixOutbox {
  inner: Outbox<JobsOutboxEvent>,
  pool: PgPool,
}

impl ObixOutbox {
  /// Initialize the obix outbox on `pool`.
  pub async fn init(pool: &PgPool, config: MailboxConfig) -> Result<Self, OutboxError> {
    let inner = Outbox::<JobsOutboxEvent>::init(pool, config)
      .await
      .map_err(|e| OutboxError::Storage(e.to_string()))?;
    Ok(Self {
      inner,
      pool: pool.clone(),
    })
  }

  /// Underlying obix outbox, for advanced listener wiring (`listen_persisted`, `listen_all`, ...).
  pub fn obix(&self) -> &Outbox<JobsOutboxEvent> {
    &self.inner
  }

  /// Shared PostgreSQL pool.
  pub fn pool(&self) -> &PgPool {
    &self.pool
  }

  /// Append an outbox row in its own transaction.
  pub fn insert(&self, record: OutboxRecord) -> Effect<OutboxRecord, OutboxError, ()> {
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

  /// Register an obix-native relay handler.
  pub async fn register_event_handler<H>(
    &self,
    jobs: &mut ::job::Jobs,
    config: OutboxEventJobConfig,
    handler: H,
  ) -> Result<(), OutboxError>
  where
    H: OutboxEventHandler<JobsOutboxEvent>,
  {
    self
      .inner
      .register_event_handler(jobs, config, handler)
      .await
      .map_err(|e| OutboxError::Storage(e.to_string()))
  }
}
