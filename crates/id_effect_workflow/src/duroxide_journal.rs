//! duroxide-pg backed durable step journal (production path).

use crate::error::WorkflowError;
use crate::journal::StepJournal;
use duroxide_pg::migrations::MigrationRunner;
use serde::{Serialize, de::DeserializeOwned};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
    .unwrap_or(0)
}

/// DDL for id_effect step cache tables (separate from duroxide orchestration schema).
pub const DUROXIDE_STEP_JOURNAL_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS id_effect_workflows (
  id TEXT PRIMARY KEY NOT NULL,
  created_ms BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS id_effect_completed_steps (
  workflow_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  step_name TEXT NOT NULL,
  output_json TEXT NOT NULL,
  completed_ms BIGINT NOT NULL,
  PRIMARY KEY (workflow_id, seq),
  FOREIGN KEY (workflow_id) REFERENCES id_effect_workflows(id)
);
"#;

/// Apply step journal DDL and duroxide-pg migrations on `pool`.
pub async fn bootstrap_duroxide_schema(pool: &PgPool) -> Result<(), WorkflowError> {
  for statement in DUROXIDE_STEP_JOURNAL_DDL
    .split(';')
    .map(str::trim)
    .filter(|s| !s.is_empty())
  {
    sqlx::query(statement)
      .execute(pool)
      .await
      .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
  }
  let runner = MigrationRunner::new(Arc::new(pool.clone()), "public".to_string());
  runner
    .migrate()
    .await
    .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
  Ok(())
}

/// PostgreSQL [`StepJournal`] backed by shared pool tables; idempotent on `(workflow_id, seq)`.
pub struct DuroxideStepJournal {
  pool: PgPool,
}

impl DuroxideStepJournal {
  /// Wrap a shared [`PgPool`] (typically from [`id_effect_sql_pg::PgPoolKey`]).
  pub fn new(pool: PgPool) -> Self {
    Self { pool }
  }

  /// Apply [`bootstrap_duroxide_schema`] synchronously (requires Tokio runtime).
  pub fn migrate_sync(&self) -> Result<(), WorkflowError> {
    tokio::task::block_in_place(|| {
      tokio::runtime::Handle::current().block_on(bootstrap_duroxide_schema(&self.pool))
    })
  }
}

impl StepJournal for DuroxideStepJournal {
  fn register_workflow(&mut self, id: &str) -> Result<(), WorkflowError> {
    if id.trim().is_empty() {
      return Err(WorkflowError::InvalidWorkflowId);
    }
    tokio::task::block_in_place(|| {
      tokio::runtime::Handle::current().block_on(async {
        sqlx::query("INSERT INTO id_effect_workflows (id, created_ms) VALUES ($1, $2)")
          .bind(id)
          .bind(now_ms())
          .execute(&self.pool)
          .await
          .map(|_| ())
          .map_err(|e| match e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
              WorkflowError::WorkflowAlreadyExists(id.to_string())
            }
            other => WorkflowError::Postgres(other.to_string()),
          })
      })
    })
  }

  fn has_workflow(&self, id: &str) -> Result<bool, WorkflowError> {
    tokio::task::block_in_place(|| {
      tokio::runtime::Handle::current().block_on(async {
        let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM id_effect_workflows WHERE id = $1")
          .bind(id)
          .fetch_one(&self.pool)
          .await
          .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
        Ok(n.0 > 0)
      })
    })
  }

  fn completed_json(&self, workflow_id: &str, seq: u32) -> Result<Option<String>, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    tokio::task::block_in_place(|| {
      tokio::runtime::Handle::current().block_on(async {
        let row: Option<(String,)> = sqlx::query_as(
          "SELECT output_json FROM id_effect_completed_steps WHERE workflow_id = $1 AND seq = $2",
        )
        .bind(workflow_id)
        .bind(seq as i32)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
        Ok(row.map(|r| r.0))
      })
    })
  }

  fn run_step_typed<T, F>(
    &mut self,
    workflow_id: &str,
    seq: u32,
    step_name: &str,
    compute: F,
  ) -> Result<T, WorkflowError>
  where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Result<T, WorkflowError>,
  {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }

    tokio::task::block_in_place(|| {
      tokio::runtime::Handle::current().block_on(async {
        let mut tx = self
          .pool
          .begin()
          .await
          .map_err(|e| WorkflowError::Postgres(e.to_string()))?;

        let existing: Option<(String,)> = sqlx::query_as(
          "SELECT output_json FROM id_effect_completed_steps WHERE workflow_id = $1 AND seq = $2 FOR UPDATE",
        )
        .bind(workflow_id)
        .bind(seq as i32)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| WorkflowError::Postgres(e.to_string()))?;

        if let Some((json,)) = existing {
          let value: T = serde_json::from_str(&json)?;
          tx.commit()
            .await
            .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
          return Ok(value);
        }

        let value = compute()?;
        let json = serde_json::to_string(&value)?;
        sqlx::query(
          "INSERT INTO id_effect_completed_steps (workflow_id, seq, step_name, output_json, completed_ms) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(workflow_id)
        .bind(seq as i32)
        .bind(step_name)
        .bind(&json)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .map_err(|e| WorkflowError::Postgres(e.to_string()))?;

        tx.commit()
          .await
          .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
        Ok(value)
      })
    })
  }

  fn completed_step_count(&self, workflow_id: &str) -> Result<u32, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    tokio::task::block_in_place(|| {
      tokio::runtime::Handle::current().block_on(async {
        let n: (i64,) =
          sqlx::query_as("SELECT COUNT(*) FROM id_effect_completed_steps WHERE workflow_id = $1")
            .bind(workflow_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| WorkflowError::Postgres(e.to_string()))?;
        Ok(n.0.try_into().unwrap_or(u32::MAX))
      })
    })
  }
}

/// duroxide runtime wrapper for worker lifecycle (optional orchestration host).
pub struct DuroxideWorkflowRuntime {
  database_url: String,
}

impl DuroxideWorkflowRuntime {
  /// Record database URL used to construct a duroxide [`duroxide_pg::PostgresProvider`].
  pub fn new(database_url: impl Into<String>) -> Self {
    Self {
      database_url: database_url.into(),
    }
  }

  /// Database URL for duroxide provider construction.
  pub fn database_url(&self) -> &str {
    &self.database_url
  }

  /// Build a duroxide-pg provider (runs duroxide migrations idempotently).
  pub async fn postgres_provider(&self) -> Result<duroxide_pg::PostgresProvider, WorkflowError> {
    duroxide_pg::PostgresProvider::new(&self.database_url)
      .await
      .map_err(|e| WorkflowError::Postgres(e.to_string()))
  }
}
