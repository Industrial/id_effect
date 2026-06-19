//! Error types for the durable workflow log.

use thiserror::Error;

/// Failure modes for workflow journals.
#[derive(Debug, Error)]
pub enum WorkflowError {
  /// SQLite driver or schema error (feature `memory`).
  #[cfg(feature = "memory")]
  #[error("sqlite: {0}")]
  Sqlite(#[from] rusqlite::Error),
  /// PostgreSQL / duroxide-pg failure (feature `duroxide`).
  #[cfg(feature = "duroxide")]
  #[error("postgres error: {0}")]
  Postgres(String),
  /// JSON serialization or deserialization error.
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
  /// Workflow id was empty or otherwise invalid for registration.
  #[error("invalid workflow id")]
  InvalidWorkflowId,
  /// Workflow registration called twice with the same id.
  #[error("workflow already exists: {0}")]
  WorkflowAlreadyExists(String),
  /// Referenced workflow was never registered.
  #[error("unknown workflow: {0}")]
  UnknownWorkflow(String),
}
