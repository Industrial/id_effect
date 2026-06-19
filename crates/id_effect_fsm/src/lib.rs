//! Typed **finite-state machines**, **sagas**, and **linear session types** for
//! [`id_effect`](https://github.com/Industrial/id_effect) programs.
//!
//! - [`StateMachine`] / [`TransitionTable`] — pure transition tables and [`StateMachine::step`]
//! - [`Interpreter`] — run transition hooks via [`id_effect::run_blocking`]
//! - [`to_mermaid`] / [`table_to_mermaid`] — export Mermaid `stateDiagram-v2` charts
//! - [`TaggedEvent`] / [`event_matcher`] — bridge events/states to [`id_effect::HasTag`] / [`id_effect::Matcher`]
//! - [`Saga`] / [`SagaStep`] — forward steps with LIFO compensation
//! - [`SessionSend`] / [`SessionRecv`] — linear session protocol markers
//! - [`register_fsm`] / [`step_durable`] — persist FSM snapshots through [`id_effect_workflow`]

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::unwrap_used,
    clippy::expect_used
  )
)]

mod error;
mod interpreter;
mod machine;
mod matcher;
mod saga;
mod session;
mod visualize;
mod workflow;

pub use error::{FsmError, SagaError};
pub use interpreter::{Interpreter, RunError, TransitionEffect};
pub use machine::{StateMachine, TransitionTable};
pub use matcher::{TaggedEvent, TaggedState, classify_event, event_matcher, state_matcher};
pub use saga::{Saga, SagaStep};
pub use session::{
  ClientPing, PingPong, PingStep, PongStep, ServerPing, SessionEnd, SessionProtocol, SessionRecv,
  SessionSend,
};
pub use visualize::{table_to_mermaid, to_mermaid, to_mermaid_display};
pub use workflow::{FsmSnapshot, WorkflowFsmError, register_fsm, restore_state, step_durable};

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Effect, effect, fail, run_blocking};
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

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

  mod machine_tests {
    use super::*;

    #[test]
    fn step_follows_table() {
      let mut m = door_machine();
      assert_eq!(m.step(DoorEvt::Open).unwrap(), Door::Open);
      assert_eq!(m.step(DoorEvt::Close).unwrap(), Door::Closed);
    }

    #[test]
    fn step_errors_on_missing_edge() {
      let mut m = door_machine();
      let err = m.step(DoorEvt::Close).unwrap_err();
      assert!(matches!(
        err,
        FsmError::NoTransition {
          state: Door::Closed,
          event: DoorEvt::Close
        }
      ));
    }
  }

  mod visualize_tests {
    use super::*;

    #[test]
    fn to_mermaid_contains_edges() {
      let m = door_machine();
      let diagram = to_mermaid(&m, |s| format!("{s:?}"), |e| format!("{e:?}"));
      assert!(diagram.contains("stateDiagram-v2"));
      assert!(diagram.contains("Closed --> Open"));
    }
  }

  mod interpreter_tests {
    use super::*;

    #[test]
    fn run_blocking_executes_transition_effect() {
      let counter = Arc::new(AtomicU32::new(0));
      let interp = Interpreter::<Door, DoorEvt, (), (), ()>::new().on_transition(
        Door::Closed,
        DoorEvt::Open,
        {
          let c = counter.clone();
          move || {
            let c = c.clone();
            Effect::new(move |_| {
              c.fetch_add(1, Ordering::SeqCst);
              Ok(())
            })
          }
        },
      );
      let mut m = door_machine();
      interp.run(&mut m, [DoorEvt::Open], ()).expect("run");
      assert_eq!(m.state(), Door::Open);
      assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
  }

  mod matcher_tests {
    use super::*;

    #[test]
    fn classify_event_by_tag() {
      let m = event_matcher::<DoorEvt, Door>()
        .tag("open", |t| {
          assert_eq!(t.event, DoorEvt::Open);
          Door::Open
        })
        .or_else(|_| Door::Closed);
      assert_eq!(classify_event(m, "open", DoorEvt::Open), Door::Open);
    }
  }

  mod saga_tests {
    use super::*;

    #[test]
    fn compensates_on_forward_failure() {
      let log = Arc::new(AtomicU32::new(0));
      let saga = Saga::new()
        .step(SagaStep::with_compensate(
          "a",
          {
            let l = log.clone();
            move || {
              let l = l.clone();
              Effect::new(move |_| {
                l.fetch_add(1, Ordering::SeqCst);
                Ok(())
              })
            }
          },
          {
            let l2 = log.clone();
            move || {
              let l2 = l2.clone();
              Effect::new(move |_| {
                l2.fetch_add(10, Ordering::SeqCst);
                Ok(())
              })
            }
          },
        ))
        .step(SagaStep::forward("b", || fail("boom")));
      let err = saga.run(()).unwrap_err();
      assert!(matches!(err, SagaError::Forward("boom")));
      assert_eq!(log.load(Ordering::SeqCst), 11);
    }
  }

  mod session_tests {
    use super::*;

    #[test]
    fn client_ping_pong_linear() {
      let send = ClientPing::new();
      let (_ping, recv) = send.send::<PingPong, PongStep>(PingPong::Ping);
      let (_pong, _end) = recv.recv::<PingPong, SessionEnd>(PingPong::Pong);
    }
  }

  mod workflow_tests {
    use super::*;
    use id_effect_workflow::DurableWorkflowLog;

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
  }

  mod effect_integration {
    use super::*;
    use id_effect_workflow::DurableWorkflowLog;

    #[test]
    fn effect_program_steps_door() {
      let program: Effect<Door, WorkflowFsmError<Door, DoorEvt>, ()> = effect! {
        let mut log = ~DurableWorkflowLog::open_in_memory();
        let mut m = door_machine();
        ~register_fsm(&mut log, "wf", &m);
        ~step_durable(&mut log, "wf", &mut m, DoorEvt::Open, "open");
        m.state()
      };
      let final_state = run_blocking(program, ()).expect("run");
      assert_eq!(final_state, Door::Open);
    }
  }
}
