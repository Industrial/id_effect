//! `Cause<E>`: the internal error channel (R1 shared idea). Typed failures, defects that keep
//! their panic payload, and interruption.

use std::any::Any;
use std::fmt;

use crate::fiber::FiberId;

/// The uninhabited error type: an `Effect<A, Never, R>` cannot fail with a typed error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Never {}

impl fmt::Display for Never {
  fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
    match *self {}
  }
}

impl std::error::Error for Never {}

/// An unexpected failure (a panic). Keeps the panic payload (`Cause::Die` in 0.6 lost it).
pub struct Defect {
  message: String,
  payload: Option<Box<dyn Any + Send>>,
}

impl Defect {
  /// A defect with a message and no payload.
  pub fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
      payload: None,
    }
  }

  /// A defect from a caught panic payload.
  pub fn from_panic(payload: Box<dyn Any + Send>) -> Self {
    let message = if let Some(s) = payload.downcast_ref::<&'static str>() {
      (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
      s.clone()
    } else {
      "panic with a non-string payload".to_string()
    };
    Self {
      message,
      payload: Some(payload),
    }
  }

  pub fn message(&self) -> &str {
    &self.message
  }

  /// The original panic payload, if this defect came from a panic.
  pub fn payload(&self) -> Option<&(dyn Any + Send)> {
    self.payload.as_deref()
  }
}

impl fmt::Debug for Defect {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Defect")
      .field("message", &self.message)
      .field("has_payload", &self.payload.is_some())
      .finish()
  }
}

impl Clone for Defect {
  /// Clones the message only; a panic payload is not `Clone`.
  fn clone(&self) -> Self {
    Self::new(self.message.clone())
  }
}

impl PartialEq for Defect {
  fn eq(&self, other: &Self) -> bool {
    self.message == other.message
  }
}

/// Why an effect did not produce a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Cause<E> {
  /// A typed, expected failure.
  Fail(E),
  /// A defect (panic), with its payload.
  Die(Defect),
  /// The fiber was interrupted.
  Interrupt(FiberId),
}

/// The outcome of running an effect.
pub type Exit<A, E> = Result<A, Cause<E>>;

impl<E> From<E> for Cause<E> {
  fn from(e: E) -> Self {
    Self::Fail(e)
  }
}

impl<E> Cause<E> {
  /// Widen the typed failure (`DbError` -> `AppError`); defects and interrupts pass through.
  pub fn widen<E2>(self) -> Cause<E2>
  where
    E: Into<E2>,
  {
    self.map_fail(Into::into)
  }

  pub fn map_fail<E2>(self, f: impl FnOnce(E) -> E2) -> Cause<E2> {
    match self {
      Self::Fail(e) => Cause::Fail(f(e)),
      Self::Die(d) => Cause::Die(d),
      Self::Interrupt(id) => Cause::Interrupt(id),
    }
  }

  /// `Err(e)` for a typed failure, `Ok(cause)` (retyped) for a defect or interrupt.
  pub fn split<E2>(self) -> Result<Cause<E2>, E> {
    match self {
      Self::Fail(e) => Err(e),
      Self::Die(d) => Ok(Cause::Die(d)),
      Self::Interrupt(id) => Ok(Cause::Interrupt(id)),
    }
  }

  pub fn failure(self) -> Option<E> {
    match self {
      Self::Fail(e) => Some(e),
      _ => None,
    }
  }

  pub fn is_interrupt(&self) -> bool {
    matches!(self, Self::Interrupt(_))
  }

  pub fn is_die(&self) -> bool {
    matches!(self, Self::Die(_))
  }
}

impl Cause<Never> {
  /// A `Cause<Never>` can only be a defect or an interrupt: retype it.
  pub fn absurd<E2>(self) -> Cause<E2> {
    self.map_fail(|n| match n {})
  }
}
