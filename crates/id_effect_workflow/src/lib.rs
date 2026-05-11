//! Durable **append-only** step log backed by SQLite, with **resume** semantics.
//!
//! This is the Phase **G2** spike from `docs/effect-ts-parity/phases/phase-g-cluster-workflow.md`:
//! persist completed step outputs keyed by `(workflow_id, seq)` so a restarted process can skip
//! re-execution. See the crate `README.md` and the mdBook chapter on durable workflow.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// `#[cfg(test)]` exercises `effect!` + Result plumbing; keep Clippy `-D warnings` green.
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::unwrap_used,
    clippy::expect_used
  )
)]

mod error;

pub use error::WorkflowError;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
    .unwrap_or(0)
}

fn init_schema(conn: &Connection) -> Result<(), WorkflowError> {
  conn.execute_batch(
    r#"
    PRAGMA foreign_keys = ON;
    PRAGMA journal_mode = WAL;
    CREATE TABLE IF NOT EXISTS workflows (
      id TEXT PRIMARY KEY NOT NULL,
      created_ms INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS completed_steps (
      workflow_id TEXT NOT NULL,
      seq INTEGER NOT NULL,
      step_name TEXT NOT NULL,
      output_json TEXT NOT NULL,
      completed_ms INTEGER NOT NULL,
      PRIMARY KEY (workflow_id, seq),
      FOREIGN KEY (workflow_id) REFERENCES workflows(id)
    );
  "#,
  )?;
  Ok(())
}

/// Append-only durable log of completed workflow steps (SQLite).
///
/// Open a log per process (or serialize writers externally). Typical usage:
///
/// 1. [`DurableWorkflowLog::open`](Self::open) (or [`open_in_memory`](Self::open_in_memory))
/// 2. [`register_workflow`](Self::register_workflow) once per workflow id
/// 3. [`run_step_typed`](Self::run_step_typed) for each logical step in stable `seq` order
pub struct DurableWorkflowLog {
  conn: Connection,
}

impl DurableWorkflowLog {
  /// Opens an in-memory database (tests and ephemeral demos).
  pub fn open_in_memory() -> Result<Self, WorkflowError> {
    let conn = Connection::open_in_memory()?;
    init_schema(&conn)?;
    Ok(Self { conn })
  }

  /// Opens (or creates) a SQLite database file at `path`.
  pub fn open(path: &Path) -> Result<Self, WorkflowError> {
    let conn = Connection::open(path)?;
    init_schema(&conn)?;
    Ok(Self { conn })
  }

  /// Registers a workflow id so steps may be appended. Fails if the id already exists.
  pub fn register_workflow(&mut self, id: &str) -> Result<(), WorkflowError> {
    if id.trim().is_empty() {
      return Err(WorkflowError::InvalidWorkflowId);
    }
    self
      .conn
      .execute(
        "INSERT INTO workflows (id, created_ms) VALUES (?1, ?2)",
        params![id, now_ms()],
      )
      .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(ferror, _)
          if ferror.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
          WorkflowError::WorkflowAlreadyExists(id.to_string())
        }
        other => other.into(),
      })?;
    Ok(())
  }

  /// Returns whether `id` was registered.
  pub fn has_workflow(&self, id: &str) -> Result<bool, WorkflowError> {
    let n: i64 = self.conn.query_row(
      "SELECT COUNT(*) FROM workflows WHERE id = ?1",
      params![id],
      |row| row.get(0),
    )?;
    Ok(n > 0)
  }

  /// Returns persisted JSON for a completed step, if any.
  pub fn completed_json(
    &self,
    workflow_id: &str,
    seq: u32,
  ) -> Result<Option<String>, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    let row = self
      .conn
      .query_row(
        "SELECT output_json FROM completed_steps WHERE workflow_id = ?1 AND seq = ?2",
        params![workflow_id, seq],
        |r| r.get::<_, String>(0),
      )
      .optional()?;
    Ok(row)
  }

  /// Runs or resumes a typed step: returns cached output when `(workflow_id, seq)` is already
  /// completed; otherwise runs `compute`, persists JSON, then returns.
  pub fn run_step_typed<T, F>(
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
    let tx = self
      .conn
      .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
      .query_row(
        "SELECT output_json FROM completed_steps WHERE workflow_id = ?1 AND seq = ?2",
        params![workflow_id, seq],
        |row| row.get(0),
      )
      .optional()?;
    if let Some(json) = existing {
      let value: T = serde_json::from_str(&json)?;
      tx.commit()?;
      return Ok(value);
    }
    let value = compute()?;
    let json = serde_json::to_string(&value)?;
    tx.execute(
      "INSERT INTO completed_steps (workflow_id, seq, step_name, output_json, completed_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
      params![workflow_id, seq, step_name, json, now_ms()],
    )?;
    tx.commit()?;
    Ok(value)
  }

  /// Count of completed steps for a workflow.
  pub fn completed_step_count(&self, workflow_id: &str) -> Result<u32, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    let n: i64 = self.conn.query_row(
      "SELECT COUNT(*) FROM completed_steps WHERE workflow_id = ?1",
      params![workflow_id],
      |row| row.get(0),
    )?;
    Ok(n.try_into().unwrap_or(u32::MAX))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Effect, effect, run_blocking};
  use rstest::rstest;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

  fn fresh_log() -> DurableWorkflowLog {
    DurableWorkflowLog::open_in_memory().expect("memory db")
  }

  mod register_workflow {
    use super::*;

    mod with_valid_input {
      use super::*;

      #[test]
      fn registers_and_has_workflow_returns_true() {
        let mut log = fresh_log();
        log.register_workflow("wf-1").expect("register");
        assert!(log.has_workflow("wf-1").expect("has"));
      }
    }

    mod with_invalid_input {
      use super::*;

      #[rstest]
      #[case::empty("")]
      #[case::whitespace_only("   ")]
      fn rejects_empty_or_whitespace_id(#[case] id: &str) {
        let mut log = fresh_log();
        let err = log.register_workflow(id).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidWorkflowId));
      }
    }

    mod with_duplicate_id {
      use super::*;

      #[test]
      fn returns_workflow_already_exists() {
        let mut log = fresh_log();
        log.register_workflow("dup").unwrap();
        let err = log.register_workflow("dup").unwrap_err();
        assert!(matches!(
          err,
          WorkflowError::WorkflowAlreadyExists(ref s) if s == "dup"
        ));
      }
    }
  }

  mod run_step_typed {
    use super::*;

    mod with_corrupt_cached_json {
      use super::*;

      #[test]
      fn returns_json_error_without_re_executing_compute() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corrupt.db");
        {
          let mut log = DurableWorkflowLog::open(&path).expect("open");
          log.register_workflow("wf").expect("reg");
          let _: serde_json::Value = log
            .run_step_typed("wf", 0, "s", || Ok(serde_json::json!({ "k": 1 })))
            .expect("first run");
        }
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        conn
          .execute(
            "UPDATE completed_steps SET output_json = 'not-json' WHERE workflow_id = 'wf' AND seq = 0",
            [],
          )
          .expect("tamper row");
        drop(conn);
        let mut log2 = DurableWorkflowLog::open(&path).expect("reopen");
        let runs = Arc::new(AtomicU32::new(0));
        let r = runs.clone();
        let err = log2
          .run_step_typed("wf", 0, "s", || {
            r.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({}))
          })
          .unwrap_err();
        assert!(matches!(err, WorkflowError::Json(_)));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
      }
    }

    mod with_cached_step {
      use super::*;

      #[test]
      fn skips_compute_when_step_already_completed() {
        let mut log = fresh_log();
        log.register_workflow("wf").unwrap();
        let runs = Arc::new(AtomicU32::new(0));
        let r1 = runs.clone();
        let a: i32 = log
          .run_step_typed("wf", 0, "s0", || {
            r1.fetch_add(1, Ordering::SeqCst);
            Ok(10)
          })
          .unwrap();
        assert_eq!(a, 10);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        let r2 = runs.clone();
        let b: i32 = log
          .run_step_typed("wf", 0, "s0", || {
            r2.fetch_add(1, Ordering::SeqCst);
            Ok(99)
          })
          .unwrap();
        assert_eq!(b, 10);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
      }
    }

    mod with_unknown_workflow {
      use super::*;

      #[test]
      fn returns_unknown_workflow() {
        let mut log = fresh_log();
        let err = log
          .run_step_typed("missing", 0, "s", || Ok(1_i32))
          .unwrap_err();
        assert!(matches!(
          err,
          WorkflowError::UnknownWorkflow(ref s) if s == "missing"
        ));
      }
    }

    mod with_compute_error {
      use super::*;

      #[test]
      fn does_not_persist_when_compute_returns_err() {
        let mut log = fresh_log();
        log.register_workflow("wf").unwrap();
        let err = log
          .run_step_typed("wf", 0, "s0", || {
            Err::<i32, WorkflowError>(WorkflowError::InvalidWorkflowId)
          })
          .unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidWorkflowId));
        assert_eq!(log.completed_step_count("wf").unwrap(), 0);
      }
    }

    mod multi_step {
      use super::*;

      #[test]
      fn runs_steps_in_order_and_counts() {
        let mut log = fresh_log();
        log.register_workflow("wf").unwrap();
        let a: i32 = log.run_step_typed("wf", 0, "a", || Ok(1)).unwrap();
        let b: i32 = log.run_step_typed("wf", 1, "b", || Ok(2)).unwrap();
        assert_eq!(a + b, 3);
        assert_eq!(log.completed_step_count("wf").unwrap(), 2);
      }
    }
  }

  mod restart_simulation {
    use super::*;

    #[test]
    fn new_connection_reuses_completed_outputs_on_disk() {
      let dir = tempfile::tempdir().expect("tempdir");
      let path = dir.path().join("wf.db");
      let counter = Arc::new(AtomicU32::new(0));
      {
        let mut log = DurableWorkflowLog::open(&path).expect("open");
        log.register_workflow("wf").expect("reg");
        let c = counter.clone();
        let v: i32 = log
          .run_step_typed("wf", 0, "first", || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(42)
          })
          .expect("step");
        assert_eq!(v, 42);
      }
      assert_eq!(counter.load(Ordering::SeqCst), 1);
      {
        let mut log2 = DurableWorkflowLog::open(&path).expect("reopen");
        let c = counter.clone();
        let v2: i32 = log2
          .run_step_typed("wf", 0, "first", || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(0)
          })
          .expect("resume");
        assert_eq!(v2, 42);
      }
      assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
  }

  mod completed_json {
    use super::*;

    mod with_missing_step {
      use super::*;

      #[test]
      fn returns_none_when_no_row() {
        let mut log = fresh_log();
        log.register_workflow("wf").unwrap();
        assert!(log.completed_json("wf", 0).unwrap().is_none());
      }
    }

    mod with_unknown_workflow {
      use super::*;

      #[test]
      fn returns_unknown_workflow() {
        let log = fresh_log();
        let err = log.completed_json("nope", 0).unwrap_err();
        assert!(matches!(err, WorkflowError::UnknownWorkflow(_)));
      }
    }
  }

  mod effect_composition {
    use super::*;

    #[test]
    fn run_blocking_executes_effect_that_uses_durable_log_in_memory() {
      let program: Effect<i32, WorkflowError, ()> = effect! {
        let mut log = ~DurableWorkflowLog::open_in_memory();
        ~log.register_workflow("w-effect");
        let v = ~log.run_step_typed("w-effect", 0, "s0", || Ok(7_i32));
        v
      };
      let got = run_blocking(program, ()).expect("run");
      assert_eq!(got, 7);
    }
  }

  mod completed_step_count {
    use super::*;

    mod with_unknown_workflow {
      use super::*;

      #[test]
      fn returns_unknown_workflow() {
        let log = fresh_log();
        let err = log.completed_step_count("no-such").unwrap_err();
        assert!(matches!(err, WorkflowError::UnknownWorkflow(_)));
      }
    }
  }

  mod register_workflow_sqlite_error {
    use super::*;

    #[test]
    fn other_sqlite_error_is_forwarded_as_sqlite_variant() {
      let mut log = fresh_log();
      // Drop the backing table so any INSERT raises a generic SQLite error
      // (not a ConstraintViolation), exercising the `other => other.into()` branch.
      log
        .conn
        .execute_batch("DROP TABLE IF EXISTS workflows;")
        .expect("drop table");
      let err = log.register_workflow("wf-x").unwrap_err();
      assert!(matches!(err, WorkflowError::Sqlite(_)));
    }
  }

  mod stress {
    use super::*;

    /// Manual / load profile; not run in default CI (`cargo test -- --ignored`).
    #[test]
    #[ignore = "stress: run locally with cargo test durable_workflow_stress_many_steps -- --ignored --nocapture"]
    fn durable_workflow_stress_many_steps() {
      let mut log = fresh_log();
      log.register_workflow("stress").unwrap();
      for i in 0u32..10_000 {
        let _: u32 = log
          .run_step_typed("stress", i, "step", || Ok(i))
          .expect("append");
      }
      assert_eq!(log.completed_step_count("stress").unwrap(), 10_000);
    }
  }
}
