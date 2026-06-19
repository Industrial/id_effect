//! Pluggable step journal backends — SQLite today, distributed spike for tomorrow.
//!
//! [`DurableWorkflowLog`] remains the default SQLite implementation. [`StepJournal`] abstracts
//! the `(workflow_id, seq)` contract so a future Postgres or RPC-backed journal can swap in
//! without changing FSM or saga call sites.

use crate::error::WorkflowError;

#[cfg(feature = "memory")]
use crate::DurableWorkflowLog;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::fmt::Debug;
#[cfg(feature = "memory")]
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Minimal contract for durable workflow step storage.
pub trait StepJournal: Send {
  /// Registers a workflow id (idempotent error if duplicate).
  fn register_workflow(&mut self, id: &str) -> Result<(), WorkflowError>;

  /// Whether `id` was registered.
  fn has_workflow(&self, id: &str) -> Result<bool, WorkflowError>;

  /// Persisted JSON for a completed step, if any.
  fn completed_json(&self, workflow_id: &str, seq: u32) -> Result<Option<String>, WorkflowError>;

  /// Runs or resumes a typed step (cache hit skips `compute`).
  fn run_step_typed<T, F>(
    &mut self,
    workflow_id: &str,
    seq: u32,
    step_name: &str,
    compute: F,
  ) -> Result<T, WorkflowError>
  where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Result<T, WorkflowError>;

  /// Count of completed steps for a workflow.
  fn completed_step_count(&self, workflow_id: &str) -> Result<u32, WorkflowError>;
}

#[cfg(feature = "memory")]
impl StepJournal for DurableWorkflowLog {
  fn register_workflow(&mut self, id: &str) -> Result<(), WorkflowError> {
    DurableWorkflowLog::register_workflow(self, id)
  }

  fn has_workflow(&self, id: &str) -> Result<bool, WorkflowError> {
    DurableWorkflowLog::has_workflow(self, id)
  }

  fn completed_json(&self, workflow_id: &str, seq: u32) -> Result<Option<String>, WorkflowError> {
    DurableWorkflowLog::completed_json(self, workflow_id, seq)
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
    DurableWorkflowLog::run_step_typed(self, workflow_id, seq, step_name, compute)
  }

  fn completed_step_count(&self, workflow_id: &str) -> Result<u32, WorkflowError> {
    DurableWorkflowLog::completed_step_count(self, workflow_id)
  }
}

/// Configuration for a future network-backed journal (documented spike — not wired to RPC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributedJournalConfig {
  /// Logical journal service name (e.g. `workflow-journal.default.svc`).
  pub endpoint: String,
  /// TLS required for remote append.
  pub require_tls: bool,
}

impl Default for DistributedJournalConfig {
  fn default() -> Self {
    Self {
      endpoint: "http://127.0.0.1:8090".to_string(),
      require_tls: false,
    }
  }
}

/// In-memory journal mimicking a **remote** backend for integration tests and ADR demos.
///
/// Production distributed workflow should use an external orchestrator or a real RPC journal;
/// this type proves the [`StepJournal`] trait is sufficient for FSM resume without SQLite.
#[derive(Debug, Default)]
pub struct NetworkJournalStub {
  config: DistributedJournalConfig,
  inner: Arc<Mutex<HashMap<String, HashMap<u32, String>>>>,
  registered: Arc<Mutex<HashMap<String, ()>>>,
}

impl NetworkJournalStub {
  /// Creates a stub with default local endpoint metadata.
  #[inline]
  pub fn new() -> Self {
    Self::with_config(DistributedJournalConfig::default())
  }

  /// Creates a stub tagged with the endpoint that a future RPC client would call.
  #[inline]
  pub fn with_config(config: DistributedJournalConfig) -> Self {
    Self {
      config,
      inner: Arc::new(Mutex::new(HashMap::new())),
      registered: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  /// Endpoint recorded for observability / config tests.
  #[inline]
  pub fn endpoint(&self) -> &str {
    &self.config.endpoint
  }

  /// Opens a SQLite-backed journal at `path` for side-by-side comparison in spikes.
  #[cfg(feature = "memory")]
  #[inline]
  pub fn open_sqlite(path: &Path) -> Result<DurableWorkflowLog, WorkflowError> {
    DurableWorkflowLog::open(path)
  }
}

impl StepJournal for NetworkJournalStub {
  fn register_workflow(&mut self, id: &str) -> Result<(), WorkflowError> {
    if id.trim().is_empty() {
      return Err(WorkflowError::InvalidWorkflowId);
    }
    let mut reg = self
      .registered
      .lock()
      .map_err(|_| WorkflowError::InvalidWorkflowId)?;
    if reg.contains_key(id) {
      return Err(WorkflowError::WorkflowAlreadyExists(id.to_string()));
    }
    reg.insert(id.to_string(), ());
    Ok(())
  }

  fn has_workflow(&self, id: &str) -> Result<bool, WorkflowError> {
    let reg = self
      .registered
      .lock()
      .map_err(|_| WorkflowError::InvalidWorkflowId)?;
    Ok(reg.contains_key(id))
  }

  fn completed_json(&self, workflow_id: &str, seq: u32) -> Result<Option<String>, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    let store = self
      .inner
      .lock()
      .map_err(|_| WorkflowError::InvalidWorkflowId)?;
    Ok(store.get(workflow_id).and_then(|m| m.get(&seq)).cloned())
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
    let _ = step_name;
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    if let Some(json) = self.completed_json(workflow_id, seq)? {
      return serde_json::from_str(&json).map_err(WorkflowError::Json);
    }
    let value = compute()?;
    let json = serde_json::to_string(&value)?;
    let mut store = self
      .inner
      .lock()
      .map_err(|_| WorkflowError::InvalidWorkflowId)?;
    store
      .entry(workflow_id.to_string())
      .or_default()
      .insert(seq, json);
    Ok(value)
  }

  fn completed_step_count(&self, workflow_id: &str) -> Result<u32, WorkflowError> {
    if !self.has_workflow(workflow_id)? {
      return Err(WorkflowError::UnknownWorkflow(workflow_id.to_string()));
    }
    let store = self
      .inner
      .lock()
      .map_err(|_| WorkflowError::InvalidWorkflowId)?;
    Ok(store.get(workflow_id).map(|m| m.len() as u32).unwrap_or(0))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicU32, Ordering};

  #[test]
  fn network_stub_skips_recompute_on_resume() {
    let mut journal = NetworkJournalStub::new();
    journal.register_workflow("remote-wf").unwrap();
    let runs = AtomicU32::new(0);
    let a: i32 = journal
      .run_step_typed("remote-wf", 0, "s0", || {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(7)
      })
      .unwrap();
    assert_eq!(a, 7);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let b: i32 = journal
      .run_step_typed("remote-wf", 0, "s0", || {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok(0)
      })
      .unwrap();
    assert_eq!(b, 7);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn distributed_config_default_endpoint() {
    let cfg = DistributedJournalConfig::default();
    assert!(!cfg.endpoint.is_empty());
    assert!(!cfg.require_tls);
  }

  #[test]
  fn network_stub_with_config_exposes_endpoint() {
    let cfg = DistributedJournalConfig {
      endpoint: "https://journal.test".into(),
      require_tls: true,
    };
    let stub = NetworkJournalStub::with_config(cfg);
    assert_eq!(stub.endpoint(), "https://journal.test");
  }

  #[test]
  fn register_empty_id_rejected() {
    let mut j = NetworkJournalStub::new();
    assert!(j.register_workflow("").is_err());
  }

  #[test]
  fn completed_json_unknown_workflow_errors() {
    let j = NetworkJournalStub::new();
    assert!(j.completed_json("missing", 0).is_err());
  }

  #[test]
  fn open_sqlite_journal_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("wf.db");
    let mut log = NetworkJournalStub::open_sqlite(&path).expect("open");
    log.register_workflow("sqlite-wf").unwrap();
    let n: u32 = log
      .run_step_typed("sqlite-wf", 0, "s", || Ok(9u32))
      .unwrap();
    assert_eq!(n, 9);
  }

  #[test]
  fn network_stub_register_duplicate_fails() {
    let mut j = NetworkJournalStub::new();
    j.register_workflow("w").unwrap();
    assert!(j.register_workflow("w").is_err());
  }

  #[test]
  fn network_stub_completed_step_count() {
    let mut j = NetworkJournalStub::new();
    j.register_workflow("w").unwrap();
    j.run_step_typed("w", 0, "a", || Ok(1u32)).unwrap();
    assert_eq!(j.completed_step_count("w").unwrap(), 1);
    assert!(j.has_workflow("w").unwrap());
  }

  #[test]
  fn sqlite_log_satisfies_step_journal() {
    fn exercise<J: StepJournal>(journal: &mut J) -> Result<(), WorkflowError> {
      journal.register_workflow("trait-wf")?;
      let v: String = journal.run_step_typed("trait-wf", 0, "x", || Ok("ok".to_string()))?;
      assert_eq!(v, "ok");
      Ok(())
    }
    let mut log = DurableWorkflowLog::open_in_memory().unwrap();
    exercise(&mut log).unwrap();
  }

  #[test]
  fn completed_step_count_unknown_workflow_errors() {
    let j = NetworkJournalStub::new();
    assert!(j.completed_step_count("missing").is_err());
  }

  #[test]
  fn run_step_unknown_workflow_errors() {
    let mut j = NetworkJournalStub::new();
    let err = j.run_step_typed("missing", 0, "s", || Ok(1u32));
    assert!(err.is_err());
  }

  #[test]
  fn has_workflow_false_for_unregistered() {
    let j = NetworkJournalStub::new();
    assert!(!j.has_workflow("nope").unwrap());
  }
}
