//! The internal error channel (R1): every runloop step yields `Result<A, Cause<E>>`.

use std::any::Any;
use std::fmt;

use crate::fiber::FiberId;

/// The uninhabited error: an `Effect<A, Never, R>` cannot fail (it can still die or be
/// interrupted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Never {}

impl fmt::Display for Never {
  fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
    match *self {}
  }
}

impl std::error::Error for Never {}

/// A defect: a panic (payload kept) or an explicit `die`.
pub struct Defect {
  message: String,
  payload: Option<Box<dyn Any + Send>>,
}

impl Defect {
  pub fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
      payload: None,
    }
  }

  /// From a `catch_unwind` payload; the payload itself is kept (D7 small win).
  pub fn from_panic(payload: Box<dyn Any + Send>) -> Self {
    let message = if let Some(s) = payload.downcast_ref::<&'static str>() {
      (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
      s.clone()
    } else {
      "panic with a non-string payload".to_owned()
    };
    Self {
      message,
      payload: Some(payload),
    }
  }

  pub fn message(&self) -> &str {
    &self.message
  }

  pub fn payload(&self) -> Option<&(dyn Any + Send)> {
    self.payload.as_deref()
  }

  pub(crate) fn into_payload(self) -> Box<dyn Any + Send> {
    self.payload.unwrap_or_else(|| Box::new(self.message))
  }
}

impl fmt::Debug for Defect {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "Defect({:?})", self.message)
  }
}

/// Why an effect did not succeed.
#[derive(Debug)]
pub enum Cause<E> {
  /// A typed, recoverable failure.
  Fail(E),
  /// A defect (panic or `die`).
  Die(Defect),
  /// The fiber was interrupted.
  Interrupt(FiberId),
}

impl<E> Cause<E> {
  pub fn map<E2>(self, f: impl FnOnce(E) -> E2) -> Cause<E2> {
    match self {
      Self::Fail(e) => Cause::Fail(f(e)),
      Self::Die(d) => Cause::Die(d),
      Self::Interrupt(id) => Cause::Interrupt(id),
    }
  }

  pub fn failure(&self) -> Option<&E> {
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

  /// A `Die`/`Interrupt` cause under another error type. `Fail` is mapped by the caller.
  pub(crate) fn retype<E2>(self) -> Result<Cause<E2>, E> {
    match self {
      Self::Fail(e) => Err(e),
      Self::Die(d) => Ok(Cause::Die(d)),
      Self::Interrupt(id) => Ok(Cause::Interrupt(id)),
    }
  }
}

impl<E: PartialEq> PartialEq for Cause<E> {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Fail(a), Self::Fail(b)) => a == b,
      (Self::Die(a), Self::Die(b)) => a.message == b.message,
      (Self::Interrupt(a), Self::Interrupt(b)) => a == b,
      _ => false,
    }
  }
}

/// The outcome of running an effect.
pub type Exit<A, E> = Result<A, Cause<E>>;

/// The shape of an [`Exit`], handed to finalizers (`acquire_use_release`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
  Success,
  Failure,
  Die,
  Interrupt,
}

impl ExitKind {
  pub fn of<A, E>(exit: &Exit<A, E>) -> Self {
    match exit {
      Ok(_) => Self::Success,
      Err(Cause::Fail(_)) => Self::Failure,
      Err(Cause::Die(_)) => Self::Die,
      Err(Cause::Interrupt(_)) => Self::Interrupt,
    }
  }

  pub fn is_success(self) -> bool {
    self == Self::Success
  }
}

/// Panic payload used to carry an interrupt out of a nested `~` bind (see `Ctx::bind`).
pub(crate) struct InterruptSignal(pub(crate) FiberId);

pub(crate) fn cause_from_panic<E>(payload: Box<dyn Any + Send>) -> Cause<E> {
  match payload.downcast::<InterruptSignal>() {
    Ok(sig) => Cause::Interrupt(sig.0),
    Err(payload) => Cause::Die(Defect::from_panic(payload)),
  }
}
