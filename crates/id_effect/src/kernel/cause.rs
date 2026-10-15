//! Bridge to `crate::failure` (`Result<A, Cause<E>>` to and from `Exit`), `InterruptSignal` and
//! `cause_from_panic`.
//!
//! Inside the kernel every step yields `Result<A, Cause<E>>` (the spike's `Exit` alias); the
//! public run edges return the root [`Exit`] enum. `Cause`, `Defect` and `Exit` are the root
//! `crate::failure` types (k-rebuild L1), so nothing here redefines them.

use std::any::Any;

use crate::failure::FiberId;
use crate::failure::{Cause, Defect, Exit};

/// Converts the internal step outcome into the public [`Exit`].
#[inline]
pub(crate) fn to_exit<A, E>(r: Result<A, Cause<E>>) -> Exit<A, E> {
  match r {
    Ok(a) => Exit::Success(a),
    Err(c) => Exit::Failure(c),
  }
}

/// Converts a public [`Exit`] into the internal step outcome.
#[allow(dead_code)] // used by k-fiber-tree / k-edge
#[inline]
pub(crate) fn from_exit<A, E>(exit: Exit<A, E>) -> Result<A, Cause<E>> {
  match exit {
    Exit::Success(a) => Ok(a),
    Exit::Failure(c) => Err(c),
  }
}

/// Maps every `Fail` leaf of `c` with `f` (composite causes keep their shape).
pub(crate) fn map_fail_mut<E, E2>(c: Cause<E>, f: &mut impl FnMut(E) -> E2) -> Cause<E2> {
  match c {
    Cause::Fail(e) => Cause::Fail(f(e)),
    Cause::Die(d) => Cause::Die(d),
    Cause::Interrupt(id) => Cause::Interrupt(id),
    Cause::Both(l, r) => {
      let l = map_fail_mut(*l, f);
      Cause::Both(Box::new(l), Box::new(map_fail_mut(*r, f)))
    }
    Cause::Then(l, r) => {
      let l = map_fail_mut(*l, f);
      Cause::Then(Box::new(l), Box::new(map_fail_mut(*r, f)))
    }
  }
}

/// Maps the `Fail` leaves of `c` with a one-shot `f` (what `map_err` takes).
///
/// The first `Fail` (depth-first, left to right) is mapped with `f`. A composite cause with more
/// than one `Fail` cannot be mapped by an `FnOnce`; each further `Fail` becomes a `Die` naming
/// that. The runloop itself never builds such causes (only `fork_all` / finalizer leaves do).
pub(crate) fn map_fail_once<E, E2>(c: Cause<E>, f: impl FnOnce(E) -> E2) -> Cause<E2> {
  let mut f = Some(f);
  map_fail_mut(c, &mut |e| match f.take() {
    Some(f) => Cause::Fail(f(e)),
    None => Cause::Die(Defect::new(
      "id_effect: map_err saw a second typed failure in a composite cause",
    )),
  })
  .flatten()
}

/// Splits a cause for typed recovery (`catch`): `Ok(e)` with the first `Fail` (depth-first)
/// when there is one, otherwise `Err` with the cause re-typed (it holds no `E`).
pub(crate) fn split_failure<E, E2>(c: Cause<E>) -> Result<E, Cause<E2>> {
  fn first<E>(c: Cause<E>) -> Result<E, Cause<E>> {
    match c {
      Cause::Fail(e) => Ok(e),
      Cause::Both(l, r) | Cause::Then(l, r) if l.contains(&|c| matches!(c, Cause::Fail(_))) => {
        drop(r);
        first(*l)
      }
      Cause::Both(_, r) | Cause::Then(_, r) if r.contains(&|c| matches!(c, Cause::Fail(_))) => {
        first(*r)
      }
      other => Err(other),
    }
  }
  first(c).map_err(|no_fail| {
    map_fail_mut(no_fail, &mut |_| -> E2 {
      unreachable!("id_effect: split_failure checked there is no Fail")
    })
  })
}

/// The shape of an outcome, handed to finalizers (`acquire_use_release`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExitKind {
  /// The effect succeeded.
  Success,
  /// The effect failed with a typed error.
  Failure,
  /// The effect died (panic or `die`).
  Die,
  /// The effect was interrupted.
  Interrupt,
}

impl ExitKind {
  /// Classifies a cause. A composite cause is `Die` if any leaf died, else `Failure` if any leaf
  /// failed, else `Interrupt`.
  pub fn of_cause<E>(c: &Cause<E>) -> Self {
    if c.contains(&|c| matches!(c, Cause::Die(_))) {
      Self::Die
    } else if c.contains(&|c| matches!(c, Cause::Fail(_))) {
      Self::Failure
    } else {
      Self::Interrupt
    }
  }

  /// Classifies a public [`Exit`].
  pub fn of<A, E>(exit: &Exit<A, E>) -> Self {
    match exit {
      Exit::Success(_) => Self::Success,
      Exit::Failure(c) => Self::of_cause(c),
    }
  }

  /// Classifies an internal step outcome.
  pub(crate) fn of_result<A, E>(r: &Result<A, Cause<E>>) -> Self {
    match r {
      Ok(_) => Self::Success,
      Err(c) => Self::of_cause(c),
    }
  }

  /// `true` for [`ExitKind::Success`].
  pub fn is_success(self) -> bool {
    self == Self::Success
  }
}

/// Panic payload that carries an interrupt out of a nested `~` bind (`Ctx::bind`, k-typed-r):
/// [`cause_from_panic`] turns it back into `Cause::Interrupt` instead of a `Die`.
pub(crate) struct InterruptSignal(pub(crate) FiberId);

/// Converts a caught panic payload into a cause. An [`InterruptSignal`] is downcast first, so it
/// never reaches [`Defect::from_panic`]; every other payload becomes `Die` with the payload kept.
pub(crate) fn cause_from_panic<E>(payload: Box<dyn Any + Send>) -> Cause<E> {
  match payload.downcast::<InterruptSignal>() {
    Ok(sig) => Cause::Interrupt(sig.0),
    Err(payload) => Cause::Die(Defect::from_panic(payload)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn panic_payloads_become_die_or_interrupt() {
    let c: Cause<()> = cause_from_panic(Box::new("boom"));
    match c {
      Cause::Die(d) => {
        assert_eq!(d.message(), "boom");
        assert_eq!(d.with_payload(|s: &&str| *s), Some("boom"));
      }
      other => panic!("expected Die, got {other:?}"),
    }
    let c: Cause<()> = cause_from_panic(Box::new(InterruptSignal(FiberId(9))));
    assert_eq!(c, Cause::Interrupt(FiberId(9)));
  }

  #[test]
  fn split_failure_finds_the_first_fail() {
    let c = Cause::both(
      Cause::<u8>::die("d"),
      Cause::then(Cause::fail(1), Cause::fail(2)),
    );
    assert_eq!(split_failure::<u8, ()>(c), Ok(1));
    let c = Cause::both(Cause::<u8>::die("d"), Cause::interrupt(FiberId(1)));
    let back = split_failure::<u8, String>(c).unwrap_err();
    assert_eq!(
      back,
      Cause::both(Cause::die("d"), Cause::interrupt(FiberId(1)))
    );
  }

  #[test]
  fn map_fail_once_maps_first_and_dies_on_extra() {
    let c = Cause::both(Cause::fail(1_u8), Cause::fail(2_u8));
    let m = map_fail_once(c, |e| u16::from(e) * 10);
    assert_eq!(
      m,
      Cause::both(
        Cause::fail(10),
        Cause::die("id_effect: map_err saw a second typed failure in a composite cause")
      )
    );
  }

  #[test]
  fn exit_kind_classifies() {
    assert_eq!(ExitKind::of(&Exit::<u8, ()>::succeed(1)), ExitKind::Success);
    assert_eq!(ExitKind::of(&Exit::<u8, u8>::fail(1)), ExitKind::Failure);
    assert_eq!(ExitKind::of_cause(&Cause::<u8>::die("x")), ExitKind::Die);
    assert_eq!(
      ExitKind::of_result::<u8, u8>(&Err(Cause::interrupt(FiberId(2)))),
      ExitKind::Interrupt
    );
    assert_eq!(
      ExitKind::of_cause(&Cause::both(
        Cause::fail(1_u8),
        Cause::interrupt(FiberId(2))
      )),
      ExitKind::Failure
    );
    assert_eq!(
      to_exit(from_exit(Exit::<u8, ()>::succeed(3))),
      Exit::succeed(3)
    );
  }
}
