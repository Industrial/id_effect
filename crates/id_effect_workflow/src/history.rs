//! The journal: [`HistoryEvent`]s, the instance rows ([`InstanceRecord`]) and the
//! [`WorkflowStore`] that persists both ([`PgWorkflowStore`](crate::PgWorkflowStore) in
//! production, [`MemoryWorkflowStore`](crate::MemoryWorkflowStore) in tests).

use std::fmt;
use std::str::FromStr;

use id_effect::{Effect, UtcDateTime};

use crate::error::WorkflowError;
use crate::lease::{EngineId, Fenced, Lease};
use crate::workflow::WorkflowId;

/// What a history event records (the `kind` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EventKind {
  /// The result of [`WorkflowContext::step`](crate::WorkflowContext::step): `{"ok": value}` or
  /// `{"err": message}`.
  Step,
  /// A [`WorkflowContext::now`](crate::WorkflowContext::now) reading: `{"at": RFC 3339}`.
  Now,
  /// A [`WorkflowContext::random`](crate::WorkflowContext::random) draw: `{"u64": "decimal"}`.
  Random,
}

impl EventKind {
  /// The `kind` column value: `step`, `now` or `random`.
  pub const fn as_str(self) -> &'static str {
    match self {
      EventKind::Step => "step",
      EventKind::Now => "now",
      EventKind::Random => "random",
    }
  }
}

impl fmt::Display for EventKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl FromStr for EventKind {
  type Err = WorkflowError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "step" => Ok(EventKind::Step),
      "now" => Ok(EventKind::Now),
      "random" => Ok(EventKind::Random),
      other => Err(WorkflowError::Decode {
        what: "history kind".into(),
        message: format!("unknown kind `{other}`"),
      }),
    }
  }
}

/// One journaled event of an instance (a `workflow_history` row). `seq` counts from 1 per
/// instance with no gaps; `(instance, seq)` is unique, so two writers can never both record a
/// position.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEvent {
  /// The position in the instance's history, from 1.
  pub seq: i64,
  /// What it records.
  pub kind: EventKind,
  /// The step name (for `now` and `random`, the kind's name).
  pub name: String,
  /// The journaled value.
  pub payload: serde_json::Value,
  /// When it was appended (the appending engine's clock).
  pub recorded_at: UtcDateTime,
}

impl HistoryEvent {
  /// `"step <name>"` for a step, the kind's name otherwise (error messages).
  pub(crate) fn describe(kind: EventKind, name: &str) -> String {
    match kind {
      EventKind::Step => format!("step {name}"),
      other => other.as_str().to_owned(),
    }
  }
}

/// An instance's lifecycle state (the `status` column).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WorkflowStatus {
  /// Started and not finished: claimable by any engine whose lease check passes.
  Running,
  /// Finished with an output.
  Completed,
  /// Finished with an error (a failed body, a nondeterministic replay, an undecodable value).
  Failed,
}

impl WorkflowStatus {
  /// The `status` column value: `running`, `completed` or `failed`.
  pub const fn as_str(self) -> &'static str {
    match self {
      WorkflowStatus::Running => "running",
      WorkflowStatus::Completed => "completed",
      WorkflowStatus::Failed => "failed",
    }
  }

  /// Whether the instance has finished.
  pub fn is_terminal(self) -> bool {
    self != WorkflowStatus::Running
  }
}

impl fmt::Display for WorkflowStatus {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

impl FromStr for WorkflowStatus {
  type Err = WorkflowError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "running" => Ok(WorkflowStatus::Running),
      "completed" => Ok(WorkflowStatus::Completed),
      "failed" => Ok(WorkflowStatus::Failed),
      other => Err(WorkflowError::Decode {
        what: "instance status".into(),
        message: format!("unknown status `{other}`"),
      }),
    }
  }
}

/// An instance as stored (a `workflow_instances` row).
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceRecord {
  /// The instance id.
  pub id: WorkflowId,
  /// The workflow it runs ([`Workflow::NAME`](crate::Workflow::NAME)).
  pub name: String,
  /// The workflow's [`VERSION`](crate::Workflow::VERSION) when the instance was started.
  pub version: String,
  /// Its state.
  pub status: WorkflowStatus,
  /// The encoded input.
  pub input: serde_json::Value,
  /// The encoded output, once completed.
  pub output: Option<serde_json::Value>,
  /// The error text, once failed.
  pub error: Option<String>,
  /// The engine holding the lease, if any.
  pub lease_owner: Option<EngineId>,
  /// When that lease runs out.
  pub lease_expires_at: Option<UtcDateTime>,
  /// The fencing token of the latest acquisition (0: never leased).
  pub fencing_token: i64,
  /// When it was started.
  pub created_at: UtcDateTime,
  /// When the row last changed.
  pub updated_at: UtcDateTime,
}

/// An instance to insert ([`WorkflowStore::create`]).
#[derive(Clone, Debug, PartialEq)]
pub struct NewInstance {
  /// The instance id.
  pub id: WorkflowId,
  /// The workflow name.
  pub name: String,
  /// The workflow version.
  pub version: String,
  /// The encoded input.
  pub input: serde_json::Value,
}

/// A lease the store granted, with what the engine needs to run the instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Claimed {
  /// The lease (with the new fencing token).
  pub lease: Lease,
  /// The workflow name.
  pub name: String,
  /// The version recorded when the instance was started.
  pub version: String,
  /// The encoded input.
  pub input: serde_json::Value,
}

/// How a fenced [`WorkflowStore::finish`] ends an instance.
#[derive(Clone, Debug, PartialEq)]
pub enum Finish {
  /// Completed with this encoded output.
  Completed(serde_json::Value),
  /// Failed with this error text.
  Failed(String),
}

/// Persistence of instances, leases and history.
///
/// Every time is passed in by the caller (the engine's [`WallClock`](id_effect::WallClock)),
/// never read from the database. The fenced methods (`append`, `renew`, `finish`, `release`) are
/// applied only while the instance row still has the lease's owner and fencing token and is
/// running; `append` and `finish` also need `now` to be before the lease's expiry. Each is one
/// atomic statement in [`PgWorkflowStore`](crate::PgWorkflowStore), so a takeover and a stale
/// append serialise on the instance row and the stale one is rejected.
pub trait WorkflowStore: Send + Sync + 'static {
  /// Inserts `instance` as running unless its id exists: `true` when inserted.
  fn create(&self, instance: NewInstance, now: UtcDateTime) -> Effect<bool, WorkflowError>;

  /// The instance `id`, if any.
  fn get(&self, id: &WorkflowId) -> Effect<Option<InstanceRecord>, WorkflowError>;

  /// Takes the lease of the running instance `id` for `owner` until `expires_at` when it is free
  /// or expired at `now`, incrementing its fencing token; `None` otherwise.
  fn acquire(
    &self,
    id: &WorkflowId,
    owner: &EngineId,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Option<Claimed>, WorkflowError>;

  /// [`acquire`](Self::acquire) on up to `limit` running instances of workflow `name` whose
  /// lease is free or expired, oldest first, skipping rows another engine is claiming.
  fn claim(
    &self,
    name: &str,
    owner: &EngineId,
    limit: u16,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Vec<Claimed>, WorkflowError>;

  /// The instance's history in `seq` order.
  fn history(&self, id: &WorkflowId) -> Effect<Vec<HistoryEvent>, WorkflowError>;

  /// Appends `event` under `lease` (fenced; also extends the lease to `extend_to`). `Lost` when
  /// the lease is no longer held or `event.seq` is already recorded.
  fn append(
    &self,
    lease: &Lease,
    event: HistoryEvent,
    extend_to: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError>;

  /// Extends `lease` to `expires_at` (fenced; `now` is the row's `updated_at`).
  fn renew(
    &self,
    lease: &Lease,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError>;

  /// Ends the instance with `outcome` and frees its lease (fenced).
  fn finish(
    &self,
    lease: &Lease,
    outcome: Finish,
    now: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError>;

  /// Frees `lease` without finishing the instance (fenced), so another engine may claim it at
  /// once.
  fn release(&self, lease: &Lease, now: UtcDateTime) -> Effect<Fenced, WorkflowError>;
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn kinds_and_statuses_round_trip_their_columns() {
    for kind in [EventKind::Step, EventKind::Now, EventKind::Random] {
      assert_eq!(kind.as_str().parse::<EventKind>(), Ok(kind));
    }
    for status in [
      WorkflowStatus::Running,
      WorkflowStatus::Completed,
      WorkflowStatus::Failed,
    ] {
      assert_eq!(status.to_string().parse::<WorkflowStatus>(), Ok(status));
    }
    assert!("sleep".parse::<EventKind>().is_err());
    assert!("done".parse::<WorkflowStatus>().is_err());
    assert!(!WorkflowStatus::Running.is_terminal() && WorkflowStatus::Failed.is_terminal());
    assert_eq!(HistoryEvent::describe(EventKind::Step, "a"), "step a");
    assert_eq!(HistoryEvent::describe(EventKind::Now, "now"), "now");
  }
}
