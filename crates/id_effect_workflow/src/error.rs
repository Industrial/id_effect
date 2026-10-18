//! [`WorkflowError`]: every way starting, running, replaying or reading a workflow fails.

use std::fmt;

use id_effect_jobs::JobError;
use id_effect_sql::SqlError;

use crate::WorkflowNameError;

/// Why a workflow operation failed.
///
/// Inside a workflow body the variants a step can produce are [`Step`](Self::Step) (a journaled
/// step failure, the same on every replay), [`Activity`](Self::Activity) (a journaled activity
/// failure), [`NonDeterministic`](Self::NonDeterministic), [`Decode`](Self::Decode),
/// [`LeaseLost`](Self::LeaseLost) and [`Waiting`](Self::Waiting); a body fails the instance on
/// purpose with [`WorkflowError::failed`]. A body that catches errors must let `LeaseLost` and
/// `Waiting` through (they stop the run; they are not failures of the instance).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkflowError {
  /// The database failed (driver, statement or decode error).
  Sql(SqlError),
  /// The job queue failed while scheduling an activity or a timer.
  Jobs(JobError),
  /// The body scheduled an activity or a timer on an engine without a job queue
  /// ([`WorkflowEngine::with_job_queue`](crate::WorkflowEngine::with_job_queue)).
  NoJobQueue,
  /// No instance has this id.
  NotFound(String),
  /// No workflow of this name is registered with the engine.
  UnknownWorkflow(String),
  /// A workflow of this name is already registered.
  DuplicateWorkflow(String),
  /// A workflow's [`NAME`](crate::Workflow::NAME) is not a valid [`WorkflowName`](crate::WorkflowName).
  InvalidName(WorkflowNameError),
  /// An instance id is empty, too long or holds a control character.
  InvalidId(String),
  /// The instance exists under another workflow name.
  IdInUse {
    /// The instance id.
    id: String,
    /// The workflow it was started as.
    workflow: String,
  },
  /// Replay diverged from the history: the code asked for another step (name or kind) at `seq`
  /// than the one journaled, or finished with history left over. The instance is failed: its
  /// code changed in a way the journal cannot replay. A body that catches it still fails the
  /// instance (every later call of the run fails with it too).
  NonDeterministic {
    /// The history position (from 1).
    seq: i64,
    /// What the history holds there (`"step charge"`, `"now"`, or `"nothing"`).
    history: String,
    /// What the code asked for there.
    code: String,
  },
  /// A journaled value, the input or the output no longer decodes with its schema.
  Decode {
    /// What failed to decode (`"input"`, `"step charge"`, …).
    what: String,
    /// The schema errors.
    message: String,
  },
  /// A step's effect failed; its message is journaled, so a replay fails the same way.
  Step {
    /// The step's name.
    step: String,
    /// The step error's `Display` text.
    message: String,
  },
  /// An activity failed for good (a discard, its attempts or its retry schedule ran out); its
  /// message is journaled, so a replay fails the same way.
  Activity {
    /// The activity's [`NAME`](crate::Activity::NAME).
    activity: String,
    /// The last failure's text.
    message: String,
  },
  /// The workflow failed: [`WorkflowError::failed`] from a body, or the recorded error of a
  /// failed instance read by [`WorkflowEngine::result`](crate::WorkflowEngine::result).
  Failed(String),
  /// This engine no longer holds the instance's lease (it expired or another engine took it
  /// over): nothing more is appended, and the run stops.
  LeaseLost,
  /// The run parked: it waits for an activity's result, a timer or a signal. The engine frees
  /// the lease and the instance resumes (replaying its history) once one arrives. Every later
  /// call on the run's [`WorkflowContext`](crate::WorkflowContext) fails with it too.
  Waiting,
}

impl WorkflowError {
  /// A workflow-level failure with `message` (fails the instance).
  pub fn failed(message: impl Into<String>) -> Self {
    Self::Failed(message.into())
  }

  /// Whether the run stopped because the lease was lost (not a failure of the instance).
  pub fn is_lease_lost(&self) -> bool {
    matches!(self, Self::LeaseLost)
  }
}

impl fmt::Display for WorkflowError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Sql(e) => write!(f, "workflow store: {e}"),
      Self::Jobs(e) => write!(f, "workflow job queue: {e}"),
      Self::NoJobQueue => f.write_str(
        "the workflow engine has no job queue for activities and timers (with_job_queue)",
      ),
      Self::NotFound(id) => write!(f, "no workflow instance `{id}`"),
      Self::UnknownWorkflow(name) => write!(f, "no workflow `{name}` is registered"),
      Self::DuplicateWorkflow(name) => write!(f, "workflow `{name}` is already registered"),
      Self::InvalidName(e) => write!(f, "invalid workflow name: {e}"),
      Self::InvalidId(why) => write!(f, "invalid workflow instance id: {why}"),
      Self::IdInUse { id, workflow } => {
        write!(f, "workflow instance `{id}` already exists as `{workflow}`")
      }
      Self::NonDeterministic { seq, history, code } => write!(
        f,
        "nondeterministic replay at history seq {seq}: the history holds {history}, the code asked for {code}"
      ),
      Self::Decode { what, message } => write!(f, "{what} does not decode: {message}"),
      Self::Step { step, message } => write!(f, "step `{step}` failed: {message}"),
      Self::Activity { activity, message } => {
        write!(f, "activity `{activity}` failed: {message}")
      }
      Self::Failed(message) => write!(f, "workflow failed: {message}"),
      Self::LeaseLost => f.write_str("the workflow instance's lease was lost"),
      Self::Waiting => {
        f.write_str("the workflow run is waiting for an activity, a timer or a signal")
      }
    }
  }
}

impl std::error::Error for WorkflowError {}

impl From<SqlError> for WorkflowError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

impl From<JobError> for WorkflowError {
  fn from(e: JobError) -> Self {
    Self::Jobs(e)
  }
}

impl From<WorkflowNameError> for WorkflowError {
  fn from(e: WorkflowNameError) -> Self {
    Self::InvalidName(e)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn waiting_activity_and_queue_errors_display() {
    let activity = WorkflowError::Activity {
      activity: "charge".into(),
      message: "declined".into(),
    };
    assert_eq!(activity.to_string(), "activity `charge` failed: declined");
    assert!(WorkflowError::Waiting.to_string().contains("waiting"));
    assert!(
      WorkflowError::NoJobQueue
        .to_string()
        .contains("with_job_queue")
    );
    let jobs = WorkflowError::from(JobError::InvalidOptions("x".into()));
    assert_eq!(
      jobs,
      WorkflowError::Jobs(JobError::InvalidOptions("x".into()))
    );
    assert!(jobs.to_string().starts_with("workflow job queue: "));
    assert!(!WorkflowError::Waiting.is_lease_lost());
  }
}
