//! Apalis PostgreSQL job queue — enqueue-only adapter.
//!
//! # Pull model vs [`JobRunner`]
//!
//! [`MemoryJobRunner`](crate::MemoryJobRunner) exposes a push/pop FIFO API suited to in-process
//! tests. [Apalis](https://docs.rs/apalis) stores tasks in PostgreSQL and **workers pull** work
//! via `WorkerBuilder` + `PostgresStorage::poll`. There is no meaningful `dequeue` on the storage
//! side — use [`ApalisJobQueue::enqueue`] to schedule work and run Apalis workers separately.
//!
//! # Pool type
//!
//! `apalis-postgres` 1.0 is built on sqlx 0.9, so the queue takes its re-exported
//! [`apalis_postgres::PgPool`], not the sqlx 0.8 pool the obix adapters share.

use std::sync::Arc;

use apalis_core::backend::TaskSink;
use apalis_postgres::{Config, PgPool, PostgresStorage};
use id_effect::next::{Effect, from_async};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::JobError;
use crate::runner::{JobRecord, JobSpec, JobState};

/// Serializable payload stored in Apalis PostgreSQL task rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApalisJobPayload {
  /// Stable job id.
  pub id: String,
  /// Logical handler name.
  pub name: String,
  /// Opaque payload bytes.
  pub payload: Vec<u8>,
}

impl From<JobSpec> for ApalisJobPayload {
  fn from(spec: JobSpec) -> Self {
    Self {
      id: spec.id,
      name: spec.name,
      payload: spec.payload,
    }
  }
}

impl From<ApalisJobPayload> for JobSpec {
  fn from(value: ApalisJobPayload) -> Self {
    Self {
      id: value.id,
      name: value.name,
      payload: value.payload,
      work_profile: None,
      target_node: None,
    }
  }
}

/// Enqueue-only job scheduling backed by Apalis PostgreSQL storage.
#[derive(Clone)]
pub struct ApalisJobQueue {
  storage: Arc<Mutex<PostgresStorage<ApalisJobPayload>>>,
  pool: PgPool,
}

impl ApalisJobQueue {
  /// Run Apalis PostgreSQL migrations on `pool`.
  pub async fn setup(pool: &PgPool) -> Result<(), JobError> {
    PostgresStorage::<()>::setup(pool)
      .await
      .map_err(|e| JobError::Storage(e.to_string()))
  }

  /// Connect to an existing pool; `queue_name` becomes the Apalis queue identifier.
  pub fn new(pool: &PgPool, queue_name: impl Into<String>) -> Self {
    let queue_name = queue_name.into();
    let config = Config::default().queue(&queue_name);
    Self {
      storage: Arc::new(Mutex::new(PostgresStorage::new(pool).with_config(config))),
      pool: pool.clone(),
    }
  }

  /// Enqueue a job for Apalis workers to pull.
  pub fn enqueue(&self, mut spec: JobSpec) -> Effect<JobRecord, JobError> {
    if spec.id.is_empty() {
      spec.id = Uuid::new_v4().to_string();
    }
    let record = JobRecord {
      spec: spec.clone(),
      state: JobState::Pending,
    };
    let storage = Arc::clone(&self.storage);
    let payload = ApalisJobPayload::from(spec);
    from_async(move || async move {
      let mut guard = storage.lock().await;
      guard
        .push(payload)
        .await
        .map_err(|e| JobError::Storage(e.to_string()))?;
      Ok(record)
    })
  }

  /// Shared sqlx pool backing this queue.
  pub fn pool(&self) -> &PgPool {
    &self.pool
  }
}

/// Enqueue-only boundary for production job backends (Apalis pull workers).
pub trait JobQueue: Send + Sync {
  /// Schedule background work; workers consume tasks out-of-band.
  fn enqueue(&self, spec: JobSpec) -> Effect<JobRecord, JobError>;
}

impl JobQueue for ApalisJobQueue {
  fn enqueue(&self, spec: JobSpec) -> Effect<JobRecord, JobError> {
    ApalisJobQueue::enqueue(self, spec)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn apalis_payload_round_trip() {
    let spec = JobSpec::new("notify", b"data");
    let payload = ApalisJobPayload::from(spec.clone());
    let back: JobSpec = payload.into();
    assert_eq!(back.name, spec.name);
    assert_eq!(back.payload, spec.payload);
  }
}
