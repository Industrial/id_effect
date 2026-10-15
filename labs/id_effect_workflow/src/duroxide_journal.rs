//! duroxide-pg backed durable step journal (production path).

use crate::error::WorkflowError;
use crate::journal::StepJournal;
use duroxide_pg::migrations::MigrationRunner;
use id_effect::{Cause, Exit};
use id_effect::{Effect, from_async};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Postgres, Transaction};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
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
  /// Wrap a shared [`PgPool`] (typically from [`id_effect_sql::pg::PgPool`]).
  pub fn new(pool: PgPool) -> Self {
    Self { pool }
  }

  /// Apply [`bootstrap_duroxide_schema`] synchronously.
  ///
  /// A blocking edge, like the [`StepJournal`] methods: call it from a plain thread, not from
  /// inside an async context.
  pub fn migrate_sync(&self) -> Result<(), WorkflowError> {
    let pool = self.pool.clone();
    run_edge(move || async move { bootstrap_duroxide_schema(&pool).await })
  }
}

/// Runs the async sqlx step `f` on a fiber of the global runtime (a tokio worker, so the
/// pool has its IO driver) and parks the calling thread until it finishes: the synchronous edge
/// behind the [`StepJournal`] impl.
///
/// Inside an async context the run is refused (`BlockOnInAsync`) and this panics, like every
/// Blocking edge. A defect re-raises its panic.
#[allow(
  clippy::disallowed_methods,
  reason = "edge: the synchronous StepJournal impl over async sqlx"
)]
fn run_edge<A, F, Fut>(f: F) -> Result<A, WorkflowError>
where
  A: Send + 'static,
  F: FnOnce() -> Fut + Send + 'static,
  Fut: Future<Output = Result<A, WorkflowError>> + Send + 'static,
{
  let step: Effect<A, WorkflowError> = from_async(f).fork().widen().flat_map(|h| h.join());
  match step.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(take_fail(cause)),
  }
}

/// The first typed failure of `cause`; a defect re-raises its panic and an interrupt panics.
fn take_fail(cause: Cause<WorkflowError>) -> WorkflowError {
  match cause {
    Cause::Fail(e) => e,
    Cause::Die(defect) => match defect.take_payload() {
      Some(payload) => resume_unwind(payload),
      None => panic!("{}", defect.message()),
    },
    Cause::Interrupt(fiber_id) => panic!("step journal edge interrupted (fiber {fiber_id:?})"),
    Cause::Both(left, right) | Cause::Then(left, right) => {
      if left.contains(&|c: &Cause<WorkflowError>| matches!(c, Cause::Fail(_))) {
        take_fail(*left)
      } else {
        take_fail(*right)
      }
    }
  }
}

fn pg_err(e: sqlx::Error) -> WorkflowError {
  WorkflowError::Postgres(e.to_string())
}

/// Rolls `tx` back on the runtime (dropping a sqlx transaction needs a tokio context). The
/// rollback's own error is ignored: the caller reports the error that caused it.
fn rollback(tx: Transaction<'static, Postgres>) {
  let _ = run_edge(move || async move { tx.rollback().await.map_err(pg_err) });
}

/// The `StepJournal` methods are **synchronous edges** over async sqlx: each statement runs on
/// a fiber of the global runtime while the calling thread parks. Call them from a plain
/// thread (for example `std::thread::spawn` or a synchronous `main`), not from inside a tokio
/// task or runtime context: there the run is refused and the call panics with the
/// `BlockOnInAsync` defect. From async code, move the journal work to a thread.
impl StepJournal for DuroxideStepJournal {
  fn register_workflow(&mut self, id: &str) -> Result<(), WorkflowError> {
    if id.trim().is_empty() {
      return Err(WorkflowError::InvalidWorkflowId);
    }
    let pool = self.pool.clone();
    let id = id.to_string();
    run_edge(move || async move {
      sqlx::query("INSERT INTO id_effect_workflows (id, created_ms) VALUES ($1, $2)")
        .bind(&id)
        .bind(now_ms())
        .execute(&pool)
        .await
        .map(|_| ())
        .map_err(|e| match e {
          sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            WorkflowError::WorkflowAlreadyExists(id.clone())
          }
          other => WorkflowError::Postgres(other.to_string()),
        })
    })
  }

  fn has_workflow(&self, id: &str) -> Result<bool, WorkflowError> {
    let pool = self.pool.clone();
    let id = id.to_string();
    run_edge(move || async move {
      let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM id_effect_workflows WHERE id = $1")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .map_err(pg_err)?;
      Ok(n.0 > 0)
    })
  }

  fn completed_json(&self, workflow_id: &str, seq: u32) -> Result<Option<String>, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    let pool = self.pool.clone();
    let workflow_id = workflow_id.to_string();
    run_edge(move || async move {
      let row: Option<(String,)> = sqlx::query_as(
        "SELECT output_json FROM id_effect_completed_steps WHERE workflow_id = $1 AND seq = $2",
      )
      .bind(&workflow_id)
      .bind(seq as i32)
      .fetch_optional(&pool)
      .await
      .map_err(pg_err)?;
      Ok(row.map(|r| r.0))
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

    // One transaction spans the lookup, `compute` (on the calling thread) and the insert, as in
    // 0.6; the transaction value moves between the edge steps.
    let pool = self.pool.clone();
    let wf = workflow_id.to_string();
    let (tx, existing) = run_edge(move || async move {
      let mut tx = pool.begin().await.map_err(pg_err)?;
      let existing: Option<(String,)> = sqlx::query_as(
        "SELECT output_json FROM id_effect_completed_steps WHERE workflow_id = $1 AND seq = $2 FOR UPDATE",
      )
      .bind(&wf)
      .bind(seq as i32)
      .fetch_optional(&mut *tx)
      .await
      .map_err(pg_err)?;
      Ok((tx, existing))
    })?;

    if let Some((json,)) = existing {
      let value: T = match serde_json::from_str(&json) {
        Ok(v) => v,
        Err(e) => {
          rollback(tx);
          return Err(e.into());
        }
      };
      run_edge(move || async move { tx.commit().await.map_err(pg_err) })?;
      return Ok(value);
    }

    let computed = catch_unwind(AssertUnwindSafe(compute));
    let value = match computed {
      Ok(Ok(v)) => v,
      Ok(Err(e)) => {
        rollback(tx);
        return Err(e);
      }
      Err(panic) => {
        rollback(tx);
        resume_unwind(panic);
      }
    };
    let json = match serde_json::to_string(&value) {
      Ok(j) => j,
      Err(e) => {
        rollback(tx);
        return Err(e.into());
      }
    };
    let wf = workflow_id.to_string();
    let step_name = step_name.to_string();
    run_edge(move || async move {
      let mut tx = tx;
      sqlx::query(
        "INSERT INTO id_effect_completed_steps (workflow_id, seq, step_name, output_json, completed_ms) VALUES ($1, $2, $3, $4, $5)",
      )
      .bind(&wf)
      .bind(seq as i32)
      .bind(&step_name)
      .bind(&json)
      .bind(now_ms())
      .execute(&mut *tx)
      .await
      .map_err(pg_err)?;
      tx.commit().await.map_err(pg_err)
    })?;
    Ok(value)
  }

  fn completed_step_count(&self, workflow_id: &str) -> Result<u32, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    let pool = self.pool.clone();
    let workflow_id = workflow_id.to_string();
    run_edge(move || async move {
      let n: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM id_effect_completed_steps WHERE workflow_id = $1")
          .bind(&workflow_id)
          .fetch_one(&pool)
          .await
          .map_err(pg_err)?;
      Ok(n.0.try_into().unwrap_or(u32::MAX))
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
