//! Bridge FSM stepping to [`id_effect_workflow::DurableWorkflowLog`].

use crate::error::FsmError;
use crate::machine::StateMachine;
use id_effect_workflow::{DurableWorkflowLog, WorkflowError};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::hash::Hash;

/// Persisted snapshot of an FSM after a step (or initial registration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsmSnapshot<S> {
  /// State after the recorded step.
  pub state: S,
  /// Monotonic step sequence stored in the workflow log.
  pub seq: u32,
}

/// Errors from durable FSM persistence or stepping.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowFsmError<S, E> {
  /// Underlying workflow log error.
  #[error(transparent)]
  Workflow(#[from] WorkflowError),
  /// Pure transition failure.
  #[error(transparent)]
  Transition(#[from] FsmError<S, E>),
}

/// Registers `workflow_id` and persists the initial [`StateMachine::state`] at seq `0`.
pub fn register_fsm<S, Ev>(
  log: &mut DurableWorkflowLog,
  workflow_id: &str,
  machine: &StateMachine<S, Ev>,
) -> Result<FsmSnapshot<S>, WorkflowFsmError<S, Ev>>
where
  S: Copy + Eq + Hash + Serialize + for<'de> Deserialize<'de>,
  Ev: Copy + Eq + Hash,
{
  log.register_workflow(workflow_id)?;
  let snapshot = FsmSnapshot {
    state: machine.state(),
    seq: 0,
  };
  log
    .run_step_typed(workflow_id, 0, "fsm_init", || Ok(snapshot.clone()))
    .map_err(WorkflowFsmError::Workflow)?;
  Ok(snapshot)
}

/// Applies one event, persists the new snapshot at the next seq, and updates `machine`.
pub fn step_durable<S, Ev>(
  log: &mut DurableWorkflowLog,
  workflow_id: &str,
  machine: &mut StateMachine<S, Ev>,
  event: Ev,
  event_name: &str,
) -> Result<FsmSnapshot<S>, WorkflowFsmError<S, Ev>>
where
  S: Copy + Eq + Hash + Debug + Serialize + for<'de> Deserialize<'de>,
  Ev: Copy + Eq + Hash,
{
  if !log.has_workflow(workflow_id)? {
    return Err(WorkflowFsmError::Workflow(WorkflowError::UnknownWorkflow(
      workflow_id.to_string(),
    )));
  }
  let next_seq = log.completed_step_count(workflow_id)?;
  let step_name = format!("fsm_{event_name}");
  let from = machine.state();
  let pending_state = machine
    .table()
    .next(from, event)
    .ok_or(FsmError::NoTransition { state: from, event })?;
  let snapshot = log
    .run_step_typed(workflow_id, next_seq, &step_name, || {
      Ok(FsmSnapshot {
        state: pending_state,
        seq: next_seq,
      })
    })
    .map_err(WorkflowFsmError::Workflow)?;
  machine.set_state(snapshot.state);
  Ok(snapshot)
}

/// Restores `machine` current state from the latest durable snapshot.
pub fn restore_state<S, Ev>(
  log: &mut DurableWorkflowLog,
  workflow_id: &str,
  machine: &mut StateMachine<S, Ev>,
) -> Result<FsmSnapshot<S>, WorkflowFsmError<S, Ev>>
where
  S: Copy + Eq + Hash + Serialize + for<'de> Deserialize<'de>,
  Ev: Copy + Eq + Hash,
{
  let count = log.completed_step_count(workflow_id)?;
  if count == 0 {
    return Err(WorkflowFsmError::Workflow(WorkflowError::UnknownWorkflow(
      workflow_id.to_string(),
    )));
  }
  let latest_seq = count - 1;
  let snap: FsmSnapshot<S> = log
    .run_step_typed(workflow_id, latest_seq, "fsm_restore", || {
      Err(WorkflowError::InvalidWorkflowId)
    })
    .map_err(WorkflowFsmError::Workflow)?;
  machine.set_state(snap.state);
  Ok(snap)
}
