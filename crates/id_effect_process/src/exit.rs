//! [`ExitReason`] — the data carried by exit signals and monitor downs.
//!
//! Exit reasons are plain data (strings for errors) so they can cross node
//! boundaries without code mobility, exactly like BEAM exit terms. The
//! normative semantics table lives in ADR 0009.

use std::fmt;

use id_effect::Cause;

/// Why a process exited. This is the payload of link exit signals and
/// monitor `Down` notifications.
///
/// `#[non_exhaustive]`: a `match` needs a wildcard arm, so later reasons are additive.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitReason {
  /// Graceful completion. Does **not** kill linked processes.
  Normal,
  /// Graceful stop requested by a supervisor. Trappable.
  Shutdown,
  /// Abnormal termination described by the process's error type
  /// (rendered to a string — exit reasons are data, not code).
  Error(String),
  /// Untrappable kill (`exit(pid, kill)` / brutal supervisor shutdown).
  Killed,
  /// The fiber was interrupted by the runtime.
  Interrupted,
  /// The target process did not exist when a link/monitor was set up.
  NoProc,
  /// The node hosting the process became unreachable (distribution layer).
  NoConnection,
  /// The process lost the cluster-wide name it carries to a conflicting registration
  /// (BEAM's `{global_name_conflict, Name}`). Abnormal and trappable.
  NameConflict(String),
}

impl ExitReason {
  /// Build an [`ExitReason::Error`] from any debuggable error value.
  pub fn error(err: impl fmt::Debug) -> Self {
    Self::Error(format!("{err:?}"))
  }

  /// `true` for reasons that propagate over links to non-trapping
  /// processes (everything except [`ExitReason::Normal`]).
  pub fn is_abnormal(&self) -> bool {
    !matches!(self, ExitReason::Normal)
  }

  /// `true` for reasons a supervisor treats as a *successful* stop when
  /// deciding `Transient` restarts (`Normal` and `Shutdown`).
  pub fn is_graceful(&self) -> bool {
    matches!(self, ExitReason::Normal | ExitReason::Shutdown)
  }

  /// Map a structured [`Cause`] into a wire-safe reason.
  pub fn from_cause<E: fmt::Debug>(cause: &Cause<E>) -> Self {
    match cause {
      Cause::Fail(e) => Self::Error(format!("{e:?}")),
      Cause::Die(defect) => Self::Error(defect.message().to_string()),
      Cause::Interrupt(_) => Self::Interrupted,
      Cause::Both(l, _) | Cause::Then(l, _) => Self::from_cause(l),
    }
  }
}

impl fmt::Display for ExitReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ExitReason::Normal => f.write_str("normal"),
      ExitReason::Shutdown => f.write_str("shutdown"),
      ExitReason::Error(e) => write!(f, "error: {e}"),
      ExitReason::Killed => f.write_str("killed"),
      ExitReason::Interrupted => f.write_str("interrupted"),
      ExitReason::NoProc => f.write_str("noproc"),
      ExitReason::NoConnection => f.write_str("noconnection"),
      ExitReason::NameConflict(name) => write!(f, "name conflict: {name}"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Defect, FiberId};

  #[test]
  fn display_renders_every_reason() {
    let cases = [
      (ExitReason::Normal, "normal"),
      (ExitReason::Shutdown, "shutdown"),
      (ExitReason::Error("bad".into()), "error: bad"),
      (ExitReason::Killed, "killed"),
      (ExitReason::Interrupted, "interrupted"),
      (ExitReason::NoProc, "noproc"),
      (ExitReason::NoConnection, "noconnection"),
      (ExitReason::NameConflict("svc".into()), "name conflict: svc"),
    ];
    for (reason, text) in cases {
      assert_eq!(reason.to_string(), text);
    }
  }

  #[test]
  fn from_cause_maps_every_cause_shape() {
    assert_eq!(
      ExitReason::from_cause(&Cause::Fail("e")),
      ExitReason::Error("\"e\"".into())
    );
    assert_eq!(
      ExitReason::from_cause(&Cause::<&str>::Die(Defect::new("defect"))),
      ExitReason::Error("defect".into())
    );
    assert_eq!(
      ExitReason::from_cause(&Cause::<&str>::Interrupt(FiberId::ROOT)),
      ExitReason::Interrupted
    );
    assert_eq!(
      ExitReason::from_cause(&Cause::both(
        Cause::<&str>::Interrupt(FiberId::ROOT),
        Cause::Fail("right")
      )),
      ExitReason::Interrupted
    );
    assert_eq!(
      ExitReason::from_cause(&Cause::then(Cause::Fail("left"), Cause::die("later"))),
      ExitReason::Error("\"left\"".into())
    );
  }

  #[test]
  fn graceful_and_abnormal_classification() {
    assert_eq!(ExitReason::error(7), ExitReason::Error("7".into()));
    assert!(!ExitReason::Normal.is_abnormal());
    assert!(ExitReason::Shutdown.is_abnormal());
    assert!(ExitReason::Shutdown.is_graceful());
    assert!(!ExitReason::Killed.is_graceful());
    assert!(ExitReason::NameConflict("n".into()).is_abnormal());
  }
}
