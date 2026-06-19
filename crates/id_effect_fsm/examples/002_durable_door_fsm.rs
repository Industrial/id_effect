//! Ex 002 — Durable FSM: door open/close transitions survive process restart.
use id_effect_fsm::{StateMachine, TransitionTable, register_fsm, restore_state, step_durable};
use id_effect_workflow::DurableWorkflowLog;
use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let db_path = env::temp_dir().join("id_effect_fsm_002_door.db");
  let _ = std::fs::remove_file(&db_path);
  {
    let mut log = DurableWorkflowLog::open(&db_path)?;
    let machine = door_machine();
    register_fsm(&mut log, "door-1", &machine)?;
    let mut machine = machine;
    step_durable(&mut log, "door-1", &mut machine, DoorEvt::Open, "open")?;
    assert_eq!(machine.state(), Door::Open);
  }
  {
    let mut log = DurableWorkflowLog::open(&db_path)?;
    let mut machine = door_machine();
    restore_state(&mut log, "door-1", &mut machine)?;
    step_durable(&mut log, "door-1", &mut machine, DoorEvt::Close, "close")?;
    assert_eq!(machine.state(), Door::Closed);
  }
  let _ = std::fs::remove_file(&db_path);
  println!("002_durable_door_fsm ok");
  Ok(())
}
