//! Typed **finite-state machines** and **sagas** for
//! [`id_effect`](https://github.com/Industrial/id_effect) programs, plus a `gen_statem`-style
//! process behaviour.
//!
//! - [`StateMachine`] / [`TransitionTable`] — pure transition tables and [`StateMachine::step`]
//! - [`Fsm`] / `#[derive(Fsm)]` — declare states, events and edges on an enum
//! - [`Interpreter`] — Effect-valued transition actions ([`Interpreter::step`] returns an
//!   [`id_effect::Effect`])
//! - [`FsmProcess`] / [`FsmHandle`] — host a machine in a supervised
//!   [`id_effect_process`] process: events are mailbox messages, state is queryable
//! - [`to_mermaid`] / [`table_to_mermaid`] — export Mermaid `stateDiagram-v2` charts
//! - [`TaggedEvent`] / [`event_matcher`] — bridge events/states to [`id_effect::HasTag`] / [`id_effect::Matcher`]
//! - [`Saga`] / [`SagaStep`] — forward steps with LIFO compensation, as one
//!   [`id_effect::Effect`]
//!
//! Transition actions and saga steps are [`id_effect::Effect`]s over a typed environment
//! `R` (an HList, or `HNil`): stepping, running and compensating are plain effect composition,
//! so nothing here blocks a thread.
//!
//! Durable snapshots of a [`StateMachine`] live in the unpublished labs crate
//! `id_effect_workflow::fsm`.

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

// Lets `#[derive(Fsm)]` (which names `::id_effect_fsm`) work inside this crate's own tests.
extern crate self as id_effect_fsm;

mod behaviour;
mod definition;
mod error;
mod interpreter;
mod machine;
mod matcher;
mod saga;
mod visualize;

pub use behaviour::{FsmCall, FsmHandle, FsmProcess, FsmReply, FsmSpec, Unhandled};
pub use definition::Fsm;
pub use error::{FsmError, SagaError};
/// `#[derive(Fsm)]` — see the [`Fsm`](trait@Fsm) trait docs for the attribute syntax.
pub use id_effect_proc_macro::Fsm;
pub use interpreter::{Interpreter, RunError, TransitionEffect};
pub use machine::{StateMachine, TransitionTable};
pub use matcher::{TaggedEvent, TaggedState, classify_event, event_matcher, state_matcher};
pub use saga::{Saga, SagaStep};
pub use visualize::{table_to_mermaid, to_mermaid, to_mermaid_display};

/// Compiles and runs the README examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#[cfg(test)]
pub(crate) mod test_support {
  //! Runs an effect at the test edge.

  use id_effect::Effect;
  use id_effect::{Cause, Exit};

  /// The effect's outcome as a `Result` over its typed error (panics on a defect or interrupt).
  pub(crate) fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(_) => panic!("effect died or was interrupted"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_support::run;
  use id_effect::{Effect, HNil, effect, fail, sync};
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    fn step_effect_runs_transition_action_then_commits() {
      let counter = Arc::new(AtomicU32::new(0));
      let interp = Interpreter::<Door, DoorEvt, (), (), HNil>::new().on_transition(
        Door::Closed,
        DoorEvt::Open,
        {
          let c = counter.clone();
          move || {
            let c = c.clone();
            sync(move || {
              c.fetch_add(1, Ordering::SeqCst);
            })
          }
        },
      );
      let program = interp.step(door_machine(), DoorEvt::Open);
      assert_eq!(counter.load(Ordering::SeqCst), 0, "effects are lazy");
      let m = run(program).expect("run");
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
      let saga = Saga::<(), (), &str, HNil>::new()
        .step(SagaStep::with_compensate(
          "a",
          {
            let l = log.clone();
            move || {
              let l = l.clone();
              sync(move || {
                l.fetch_add(1, Ordering::SeqCst);
              })
            }
          },
          {
            let l2 = log.clone();
            move || {
              let l2 = l2.clone();
              sync(move || {
                l2.fetch_add(10, Ordering::SeqCst);
              })
            }
          },
        ))
        .step(SagaStep::forward("b", || fail("boom")));
      let err = run(saga.run()).unwrap_err();
      assert!(matches!(err, SagaError::Forward("boom")));
      assert_eq!(log.load(Ordering::SeqCst), 11);
    }
  }

  mod derive_tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Light {
      Tick,
      Fault,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Fsm)]
    #[fsm(event = Light)]
    enum Signal {
      #[fsm(on(Tick => Green, Fault => Blinking))]
      Red,
      #[fsm(on(Tick => Amber))]
      Green,
      #[fsm(on(Tick => Red))]
      Amber,
      Blinking,
    }

    #[test]
    fn derived_machine_cycles_and_defaults_initial_to_first_variant() {
      assert_eq!(Signal::initial(), Signal::Red);
      let mut m = Signal::state_machine();
      for expected in [Signal::Green, Signal::Amber, Signal::Red] {
        assert_eq!(m.step(Light::Tick), Ok(expected));
      }
      assert_eq!(m.step(Light::Fault), Ok(Signal::Blinking));
      assert!(m.step(Light::Tick).is_err(), "Blinking is terminal");
      assert_eq!(Signal::transition_table().len(), 4);
    }
  }

  mod effect_integration {
    use super::*;

    #[test]
    fn effect_program_steps_door() {
      let program: Effect<Door, FsmError<Door, DoorEvt>> = effect!({
        let mut m = door_machine();
        m.step(DoorEvt::Open)?;
        m.state()
      });
      let final_state = run(program).expect("run");
      assert_eq!(final_state, Door::Open);
    }
  }
}
