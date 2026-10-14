//! Effect-native FSM runner: transitions with [`Effect`]-valued actions.
//!
//! The machine is threaded **by value** through the returned effects (the same
//! ownership discipline as `id_effect_process` behaviours), so no step ever nests
//! a blocking interpreter inside another effect.

use crate::error::FsmError;
use crate::machine::StateMachine;
use id_effect::{Effect, box_future};
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

/// Factory for a transition action (rebuilt on each step because [`Effect`] is not `Clone`).
pub type TransitionEffect<A, Err, R> = Arc<dyn Fn() -> Effect<A, Err, R> + Send + Sync>;

type ActionMap<S, E, A, Err, R> = HashMap<(S, E), TransitionEffect<A, Err, R>>;

/// Maps `(state, event)` pairs to effect factories run as part of the transition.
///
/// Cheap to clone (the action map is shared behind an [`Arc`]).
pub struct Interpreter<S, E, A, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  A: 'static,
  Err: 'static,
  R: 'static,
{
  actions: Arc<ActionMap<S, E, A, Err, R>>,
}

impl<S, E, A, Err, R> Clone for Interpreter<S, E, A, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  A: 'static,
  Err: 'static,
  R: 'static,
{
  fn clone(&self) -> Self {
    Self {
      actions: Arc::clone(&self.actions),
    }
  }
}

impl<S, E, A, Err, R> Default for Interpreter<S, E, A, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  A: 'static,
  Err: 'static,
  R: 'static,
{
  fn default() -> Self {
    Self {
      actions: Arc::new(HashMap::new()),
    }
  }
}

impl<S, E, A, Err, R> Interpreter<S, E, A, Err, R>
where
  S: Copy + Eq + Hash + Debug + 'static,
  E: Copy + Eq + Hash + Debug + 'static,
  A: 'static,
  Err: 'static,
  R: 'static,
{
  /// Empty interpreter (pure stepping only).
  pub fn new() -> Self {
    Self::default()
  }

  /// Registers an action factory for `(from, event)`. Later calls for the same pair overwrite.
  pub fn on_transition<F>(mut self, from: S, event: E, make: F) -> Self
  where
    F: Fn() -> Effect<A, Err, R> + Send + Sync + 'static,
  {
    Arc::make_mut(&mut self.actions).insert((from, event), Arc::new(make));
    self
  }

  /// Whether an action is registered for `(from, event)`.
  pub fn has_action(&self, from: S, event: E) -> bool {
    self.actions.contains_key(&(from, event))
  }

  /// Fresh action effect for `(from, event)`, if one is registered.
  pub(crate) fn action(&self, from: S, event: E) -> Option<Effect<A, Err, R>> {
    self.actions.get(&(from, event)).map(|make| make())
  }

  /// Applies one `event` to `machine`.
  ///
  /// The table is consulted **first**: a missing edge fails with
  /// [`RunError::Transition`] without running any action. Otherwise the registered action
  /// (if any) runs, and the new state is committed only after it succeeds — an action
  /// failure ([`RunError::Effect`]) leaves no half-applied transition behind. The machine
  /// is consumed; clone it beforehand if you need to retry from the old state.
  pub fn step(
    &self,
    machine: StateMachine<S, E>,
    event: E,
  ) -> Effect<StateMachine<S, E>, RunError<S, E, Err>, R> {
    step_with(&self.actions, machine, event)
  }

  /// Applies `events` in order, stopping at the first failure; yields the final machine.
  pub fn run<I>(
    &self,
    machine: StateMachine<S, E>,
    events: I,
  ) -> Effect<StateMachine<S, E>, RunError<S, E, Err>, R>
  where
    I: IntoIterator<Item = E>,
  {
    let actions = Arc::clone(&self.actions);
    let events: Vec<E> = events.into_iter().collect();
    Effect::new_async(move |env: &mut R| {
      box_future(async move {
        let mut machine = machine;
        for event in events {
          machine = step_with(&actions, machine, event).run(env).await?;
        }
        Ok(machine)
      })
    })
  }
}

fn step_with<S, E, A, Err, R>(
  actions: &ActionMap<S, E, A, Err, R>,
  mut machine: StateMachine<S, E>,
  event: E,
) -> Effect<StateMachine<S, E>, RunError<S, E, Err>, R>
where
  S: Copy + Eq + Hash + Debug + 'static,
  E: Copy + Eq + Hash + Debug + 'static,
  A: 'static,
  Err: 'static,
  R: 'static,
{
  let from = machine.state();
  if machine.table().next(from, event).is_none() {
    return Effect::new(move |_| {
      Err(RunError::Transition(FsmError::NoTransition {
        state: from,
        event,
      }))
    });
  }
  let action = actions.get(&(from, event)).map(|make| make());
  Effect::new_async(move |env: &mut R| {
    box_future(async move {
      if let Some(action) = action {
        action.run(env).await.map_err(RunError::Effect)?;
      }
      machine.step(event).map_err(RunError::Transition)?;
      Ok(machine)
    })
  })
}

/// Combined failure from pure stepping or effect execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError<S, E, Err> {
  /// Transition table miss.
  Transition(FsmError<S, E>),
  /// Registered action returned `Err`.
  Effect(Err),
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::machine::TransitionTable;
  use id_effect::{fail, run_blocking, succeed};
  use std::sync::Mutex;

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  enum S {
    A,
    B,
    C,
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  enum E {
    Next,
  }

  fn machine() -> StateMachine<S, E> {
    StateMachine::new(
      S::A,
      TransitionTable::new()
        .on(S::A, E::Next, S::B)
        .on(S::B, E::Next, S::C),
    )
  }

  #[test]
  fn missing_edge_fails_without_running_action() {
    let ran = Arc::new(Mutex::new(false));
    let flag = Arc::clone(&ran);
    let interp = Interpreter::<S, E, (), &str, ()>::new().on_transition(S::C, E::Next, move || {
      *flag.lock().unwrap() = true;
      succeed(())
    });
    let mut m = machine();
    m.set_state(S::C);
    let err = run_blocking(interp.step(m, E::Next), ()).unwrap_err();
    assert_eq!(
      err,
      RunError::Transition(FsmError::NoTransition {
        state: S::C,
        event: E::Next
      })
    );
    assert!(!*ran.lock().unwrap());
  }

  #[test]
  fn action_failure_surfaces_as_effect_error() {
    let interp = Interpreter::<S, E, (), &str, ()>::new()
      .on_transition(S::A, E::Next, || fail("action failed"));
    let err = run_blocking(interp.step(machine(), E::Next), ()).unwrap_err();
    assert_eq!(err, RunError::Effect("action failed"));
  }

  #[test]
  fn run_threads_environment_through_every_action() {
    let interp = Interpreter::<S, E, (), (), Vec<S>>::new()
      .on_transition(S::A, E::Next, || {
        Effect::new(|log: &mut Vec<S>| {
          log.push(S::A);
          Ok(())
        })
      })
      .on_transition(S::B, E::Next, || {
        Effect::new(|log: &mut Vec<S>| {
          log.push(S::B);
          Ok(())
        })
      });
    assert!(interp.has_action(S::A, E::Next));
    let program = interp
      .run(machine(), [E::Next, E::Next])
      .flat_map(|m| Effect::new(move |log: &mut Vec<S>| Ok((m.state(), log.clone()))));
    let (state, log) = run_blocking(program, Vec::new()).unwrap();
    assert_eq!(state, S::C);
    assert_eq!(log, vec![S::A, S::B]);
  }

  #[test]
  fn run_stops_at_first_failure() {
    let interp = Interpreter::<S, E, (), (), ()>::new();
    let err = run_blocking(interp.run(machine(), [E::Next, E::Next, E::Next]), ()).unwrap_err();
    assert!(matches!(
      err,
      RunError::Transition(FsmError::NoTransition { state: S::C, .. })
    ));
  }

  #[test]
  fn cloned_interpreter_shares_actions() {
    let interp =
      Interpreter::<S, E, (), (), ()>::new().on_transition(S::A, E::Next, || succeed(()));
    let copy = interp.clone();
    assert!(copy.has_action(S::A, E::Next));
    assert!(!copy.has_action(S::B, E::Next));
  }
}
