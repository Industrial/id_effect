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
  }

  #[test]
  fn outbox_error_display_variants() {
    assert!(OutboxError::NotFound("o".into()).to_string().contains("o"));
    assert!(OutboxError::Lock("l".into()).to_string().contains("l"));
  }
}
