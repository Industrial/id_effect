//! FSM and saga error types.

use thiserror::Error;

/// Errors from pure transition lookup or effectful interpretation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FsmError<S, E> {
  /// No edge exists for `(state, event)`.
  #[error("no transition from {state:?} on event {event:?}")]
  NoTransition {
    /// Current state when the event was applied.
    state: S,
    /// Event that had no outgoing edge.
    event: E,
  },
}

/// Errors from saga forward or compensation phases.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SagaError<E> {
  /// A forward step failed before completion.
  #[error("forward step failed: {0}")]
  Forward(E),
  /// A compensation step failed during rollback.
  #[error("compensation failed: {0}")]
  Compensate(E),
}
