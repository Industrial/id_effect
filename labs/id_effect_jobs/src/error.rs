//! Error types for job queue and outbox operations.

use thiserror::Error;

/// Job runner failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum JobError {
  /// Mutex poison or other internal lock failure.
  #[error("internal lock error: {0}")]
  Lock(String),
  /// No job with the given id exists.
  #[error("job not found: {0}")]
  NotFound(String),
  /// Job was already dequeued or completed.
  #[error("job already processed: {0}")]
  AlreadyProcessed(String),
  /// Backend storage or broker failure.
  #[error("storage error: {0}")]
  Storage(String),
  /// Operation not supported by this backend (e.g. Apalis pull-model workers).
  #[error("unsupported: {0}")]
  Unsupported(String),
}

/// Outbox table failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OutboxError {
  /// Mutex poison or other internal lock failure.
  #[error("internal lock error: {0}")]
  Lock(String),
  /// No outbox row with the given id exists.
  #[error("outbox record not found: {0}")]
  NotFound(String),
  /// PostgreSQL / obix backend failure.
  #[error("storage error: {0}")]
  Storage(String),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn job_error_display_variants() {
    assert!(JobError::NotFound("j".into()).to_string().contains("j"));
    assert!(JobError::Lock("l".into()).to_string().contains("l"));
    assert!(
      JobError::AlreadyProcessed("x".into())
        .to_string()
        .contains("x")
    );
    assert!(JobError::Storage("db".into()).to_string().contains("db"));
  }

  #[test]
  fn outbox_error_display_variants() {
    assert!(OutboxError::NotFound("o".into()).to_string().contains("o"));
    assert!(OutboxError::Lock("l".into()).to_string().contains("l"));
    assert!(OutboxError::Storage("pg".into()).to_string().contains("pg"));
  }
}
