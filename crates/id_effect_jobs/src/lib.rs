//! Background jobs for id_effect services: a Postgres `SKIP LOCKED` queue on `id_effect_sql`,
//! workers, retries with backoff and cron schedules.
//!
//! [`JobState`] is the lifecycle every queued job moves through; [`JobState::as_str`] is the value
//! stored in the queue table's `state` column.
//!
//! ```
//! use id_effect_jobs::JobState;
//!
//! let state: JobState = "failed".parse().unwrap();
//! assert!(!state.is_terminal());
//! assert_eq!(state.as_str(), "failed");
//! ```
//!
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;
use std::str::FromStr;

/// Lifecycle of a queued job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobState {
  /// Waiting for its `run_at` time and a free worker.
  Scheduled,
  /// Claimed by a worker.
  Running,
  /// Finished successfully.
  Succeeded,
  /// Failed; runs again after its backoff unless its attempts are exhausted.
  Failed,
  /// Out of attempts; kept for inspection and a manual retry.
  Dead,
}

impl JobState {
  /// Every state, in lifecycle order.
  pub const ALL: [JobState; 5] = [
    JobState::Scheduled,
    JobState::Running,
    JobState::Succeeded,
    JobState::Failed,
    JobState::Dead,
  ];

  /// The column value: `scheduled`, `running`, `succeeded`, `failed` or `dead`.
  pub const fn as_str(self) -> &'static str {
    match self {
      JobState::Scheduled => "scheduled",
      JobState::Running => "running",
      JobState::Succeeded => "succeeded",
      JobState::Failed => "failed",
      JobState::Dead => "dead",
    }
  }

  /// Whether no worker picks the job up again from this state without a manual retry.
  pub const fn is_terminal(self) -> bool {
    matches!(self, JobState::Succeeded | JobState::Dead)
  }
}

impl fmt::Display for JobState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// A `state` column value that names no [`JobState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownJobState(pub String);

impl fmt::Display for UnknownJobState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "unknown job state `{}`", self.0)
  }
}

impl std::error::Error for UnknownJobState {}

impl FromStr for JobState {
  type Err = UnknownJobState;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    JobState::ALL
      .into_iter()
      .find(|state| state.as_str() == s)
      .ok_or_else(|| UnknownJobState(s.to_owned()))
  }
}
