//! Pure transition tables and [`StateMachine`] stepping.

use crate::error::FsmError;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

/// Immutable edge map keyed by `(state, event)`.
#[derive(Debug, Clone, Default)]
pub struct TransitionTable<S, E> {
  edges: HashMap<(S, E), S>,
}

impl<S, E> TransitionTable<S, E>
where
  S: Copy + Eq + Hash,
  E: Copy + Eq + Hash,
{
  /// Empty table.
  pub fn new() -> Self {
    Self {
      edges: HashMap::new(),
    }
  }

  /// Records `from --event--> to`. Later calls for the same pair overwrite.
  pub fn on(mut self, from: S, event: E, to: S) -> Self {
    self.edges.insert((from, event), to);
    self
  }

  /// Resolves the next state, if any.
  pub fn next(&self, from: S, event: E) -> Option<S> {
    self.edges.get(&(from, event)).copied()
  }

  /// Iterator over `(from, event, to)` triples (stable order unspecified).
  pub fn edges(&self) -> impl Iterator<Item = (S, E, S)> + '_ {
    self
      .edges
      .iter()
      .map(|((from, event), to)| (*from, *event, *to))
  }

  /// Number of edges.
  pub fn len(&self) -> usize {
    self.edges.len()
  }

  /// Whether the table has no edges.
  pub fn is_empty(&self) -> bool {
    self.edges.is_empty()
  }
}

/// Mutable FSM instance: holds current state and a shared transition table.
#[derive(Debug, Clone)]
pub struct StateMachine<S, E> {
  initial: S,
  current: S,
  table: TransitionTable<S, E>,
}

impl<S, E> StateMachine<S, E>
where
  S: Copy + Eq + Hash,
  E: Copy + Eq + Hash,
{
  /// Builds a machine starting at `initial` with `table`.
  pub fn new(initial: S, table: TransitionTable<S, E>) -> Self {
    Self {
      initial,
      current: initial,
      table,
    }
  }

  /// Current state.
  pub fn state(&self) -> S {
    self.current
  }

  /// Initial state configured at construction.
  pub fn initial(&self) -> S {
    self.initial
  }

  /// Shared transition table.
  pub fn table(&self) -> &TransitionTable<S, E> {
    &self.table
  }

  /// Resets current state to [`Self::initial`].
  pub fn reset(&mut self) {
    self.current = self.initial;
  }

  /// Sets current state (e.g. after loading a durable snapshot).
  pub fn set_state(&mut self, state: S) {
    self.current = state;
  }
}

impl<S, E> StateMachine<S, E>
where
  S: Copy + Eq + Hash + Debug,
  E: Copy + Eq + Hash + Debug,
{
  /// Applies one event, updating [`Self::state`] on success.
  pub fn step(&mut self, event: E) -> Result<S, FsmError<S, E>> {
    let from = self.current;
    match self.table.next(from, event) {
      Some(to) => {
        self.current = to;
        Ok(to)
      }
      None => Err(FsmError::NoTransition { state: from, event }),
    }
  }
}
