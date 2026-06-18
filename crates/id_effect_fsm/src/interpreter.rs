//! Effectful FSM runner via [`id_effect::run_blocking`].

use crate::error::FsmError;
use crate::machine::StateMachine;
use id_effect::{Effect, run_blocking};
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

/// Factory for a transition effect (rebuilt on each step because [`Effect`] is not `Clone`).
pub type TransitionEffect<A, Err, R> = Box<dyn Fn() -> Effect<A, Err, R> + Send + Sync>;

/// Maps `(state, event)` pairs to effect factories run via [`run_blocking`] after stepping.
pub struct Interpreter<S, E, A, Err, R>
where
  S: Copy + Eq + Hash + 'static,
  E: Copy + Eq + Hash + 'static,
  A: 'static,
  Err: 'static,
  R: 'static,
{
  actions: HashMap<(S, E), TransitionEffect<A, Err, R>>,
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
      actions: HashMap::new(),
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

  /// Registers an effect factory for `(from, event)`.
  pub fn on_transition<F>(mut self, from: S, event: E, make: F) -> Self
  where
    F: Fn() -> Effect<A, Err, R> + Send + Sync + 'static,
  {
    self.actions.insert((from, event), Box::new(make));
    self
  }

  /// Steps the machine on `event`, runs a registered effect (if any) via `run_blocking`.
  pub fn step(
    &self,
    machine: &mut StateMachine<S, E>,
    event: E,
    env: R,
  ) -> Result<S, RunError<S, E, Err>>
  where
    R: Clone,
  {
    let from = machine.state();
    if let Some(make) = self.actions.get(&(from, event)) {
      run_blocking(make(), env.clone()).map_err(RunError::Effect)?;
    }
    machine.step(event).map_err(RunError::Transition)
  }

  /// Applies `events` in order; returns the final state.
  pub fn run<I>(
    &self,
    machine: &mut StateMachine<S, E>,
    events: I,
    env: R,
  ) -> Result<S, RunError<S, E, Err>>
  where
    I: IntoIterator<Item = E>,
    R: Clone,
  {
    for event in events {
      self.step(machine, event, env.clone())?;
    }
    Ok(machine.state())
  }
}

/// Combined failure from pure stepping or effect execution.
#[derive(Debug, PartialEq, Eq)]
pub enum RunError<S, E, Err> {
  /// Transition table miss.
  Transition(FsmError<S, E>),
  /// Registered effect returned `Err`.
  Effect(Err),
}
