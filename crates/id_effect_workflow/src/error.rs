//! [`WorkflowError`]: every way starting, running, replaying or reading a workflow fails.

use std::fmt;

use id_effect_sql::SqlError;

use crate::WorkflowNameError;

/// Why a workflow operation failed.
///
/// Inside a workflow body the variants a step can produce are [`Step`](Self::Step) (a journaled
/// step failure, the same on every replay), [`NonDeterministic`](Self::NonDeterministic),
/// [`Decode`](Self::Decode) and [`LeaseLost`](Self::LeaseLost); a body fails the instance on
/// purpose with [`WorkflowError::failed`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkflowError {
  /// The database failed (driver, statement or decode error).
  Sql(SqlError),
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
  /// code changed in a way the journal cannot replay.
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
  /// The workflow failed: [`WorkflowError::failed`] from a body, or the recorded error of a
  /// failed instance read by [`WorkflowEngine::result`](crate::WorkflowEngine::result).
  Failed(String),
  /// This engine no longer holds the instance's lease (it expired or another engine took it
  /// over): nothing more is appended, and the run stops.
  LeaseLost,
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
      Self::Failed(message) => write!(f, "workflow failed: {message}"),
      Self::LeaseLost => f.write_str("the workflow instance's lease was lost"),
    }
  }
}

impl std::error::Error for WorkflowError {}

impl From<SqlError> for WorkflowError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

impl From<WorkflowNameError> for WorkflowError {
  fn from(e: WorkflowNameError) -> Self {
    Self::InvalidName(e)
  }
}
