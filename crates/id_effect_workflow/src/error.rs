//! Error types for the durable workflow log.

use thiserror::Error;

/// Failure modes for [`DurableWorkflowLog`](crate::DurableWorkflowLog).
#[derive(Debug, Error)]
pub enum WorkflowError {
  /// SQLite driver or schema error.
  #[error("sqlite: {0}")]
  Sqlite(#[from] rusqlite::Error),
  /// JSON serialization or deserialization error.
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
  /// Workflow id was empty or otherwise invalid for registration.
  #[error("invalid workflow id")]
  InvalidWorkflowId,
  /// [`DurableWorkflowLog::register_workflow`](crate::DurableWorkflowLog::register_workflow) called twice with the same id.
  #[error("workflow already exists: {0}")]
  WorkflowAlreadyExists(String),
  /// Referenced workflow was never registered.
  #[error("unknown workflow: {0}")]
  UnknownWorkflow(String),
}
