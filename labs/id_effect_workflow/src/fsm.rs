//! Durable [`id_effect_fsm`] state machines: persist FSM snapshots through any
//! [`StepJournal`].
//!
//! Moved here from `id_effect_fsm` in 0.6 (ADR 0010 Amendment 1) so the published
//! fsm crate carries no dependency on this unpublished labs crate.

use crate::{StepJournal, WorkflowError};
use id_effect_fsm::{FsmError, StateMachine};
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
pub fn register_fsm<S, Ev, J>(
  log: &mut J,
  workflow_id: &str,
  machine: &StateMachine<S, Ev>,
) -> Result<FsmSnapshot<S>, WorkflowFsmError<S, Ev>>
where
  S: Copy + Eq + Hash + Serialize + for<'de> Deserialize<'de>,
  Ev: Copy + Eq + Hash,
  J: StepJournal + ?Sized,
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
pub fn step_durable<S, Ev, J>(
  log: &mut J,
  workflow_id: &str,
  machine: &mut StateMachine<S, Ev>,
  event: Ev,
  event_name: &str,
) -> Result<FsmSnapshot<S>, WorkflowFsmError<S, Ev>>
where
  S: Copy + Eq + Hash + Debug + Serialize + for<'de> Deserialize<'de>,
  Ev: Copy + Eq + Hash,
  J: StepJournal + ?Sized,
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
pub fn restore_state<S, Ev, J>(
  log: &mut J,
  workflow_id: &str,
  machine: &mut StateMachine<S, Ev>,
) -> Result<FsmSnapshot<S>, WorkflowFsmError<S, Ev>>
where
  S: Copy + Eq + Hash + Serialize + for<'de> Deserialize<'de>,
  Ev: Copy + Eq + Hash,
  J: StepJournal + ?Sized,
{
  let count = log.completed_step_count(workflow_id)?;
  if count == 0 {
    return Err(WorkflowFsmError::Workflow(WorkflowError::UnknownWorkflow(
      workflow_id.to_string(),
    )));
  }
  let latest_seq = count - 1;
  let json = log
    .completed_json(workflow_id, latest_seq)?
    .ok_or(WorkflowFsmError::Workflow(WorkflowError::InvalidWorkflowId))?;
  let snap: FsmSnapshot<S> =
    serde_json::from_str(&json).map_err(|e| WorkflowFsmError::Workflow(WorkflowError::Json(e)))?;
  machine.set_state(snap.state);
  Ok(snap)
}

#[cfg(all(test, feature = "memory"))]
mod tests {
  use super::*;
  use crate::DurableWorkflowLog;
  use id_effect::{Effect, effect};
  use id_effect_fsm::TransitionTable;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
  enum Door {
    Closed,
    Open,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  enum DoorEvt {
    Open,
    Close,
  }

  fn door_machine() -> StateMachine<Door, DoorEvt> {
    let table = TransitionTable::new()
      .on(Door::Closed, DoorEvt::Open, Door::Open)
      .on(Door::Open, DoorEvt::Close, Door::Closed);
    StateMachine::new(Door::Closed, table)
  }

  #[test]
  fn register_fsm_duplicate_errors() {
    let mut log = DurableWorkflowLog::open_in_memory().unwrap();
    let m = door_machine();
    register_fsm(&mut log, "wf", &m).unwrap();
    let err = register_fsm(&mut log, "wf", &m).unwrap_err();
    assert!(matches!(err, WorkflowFsmError::Workflow(_)));
  }

  #[test]
  fn step_durable_unknown_workflow_errors() {
    let mut log = DurableWorkflowLog::open_in_memory().unwrap();
    let mut m = door_machine();
    let err = step_durable(&mut log, "missing", &mut m, DoorEvt::Open, "open").unwrap_err();
    assert!(matches!(err, WorkflowFsmError::Workflow(_)));
  }

  #[test]
  fn restore_unknown_workflow_errors() {
    let mut log = DurableWorkflowLog::open_in_memory().unwrap();
    let mut m = door_machine();
    let err = restore_state(&mut log, "missing", &mut m).unwrap_err();
    assert!(matches!(err, WorkflowFsmError::Workflow(_)));
  }

  #[test]
  fn durable_step_persists_state() {
    let mut log = DurableWorkflowLog::open_in_memory().unwrap();
    let mut m = door_machine();
    register_fsm(&mut log, "door", &m).unwrap();
    let snap = step_durable(&mut log, "door", &mut m, DoorEvt::Open, "open").unwrap();
    assert_eq!(snap.state, Door::Open);
    let mut m2 = door_machine();
    let restored = restore_state(&mut log, "door", &mut m2).unwrap();
    assert_eq!(restored.state, Door::Open);
    assert_eq!(m2.state(), Door::Open);
  }

  #[test]
  fn effect_program_steps_door() {
    let program: Effect<Door, WorkflowFsmError<Door, DoorEvt>> = effect!({
      let mut log = DurableWorkflowLog::open_in_memory()?;
      let mut m = door_machine();
      register_fsm(&mut log, "wf", &m)?;
      step_durable(&mut log, "wf", &mut m, DoorEvt::Open, "open")?;
      m.state()
    });
    let final_state = program.block_on().into_result().expect("run");
    assert_eq!(final_state, Door::Open);
  }
}
