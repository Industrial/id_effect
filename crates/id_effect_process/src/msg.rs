//! Message envelopes and error types for the process layer.

use std::fmt;

use id_effect::Deferred;

use crate::exit::ExitReason;
use crate::pid::{MonitorRef, Pid};
use crate::process::Process;

/// System-level notification delivered to a process mailbox.
///
/// These arrive through [`crate::process::Process::handle_info`] wrapped in
/// [`Info`]. Like BEAM, **trapped** exit signals and monitor downs are
/// ordinary mailbox messages — they queue in order behind user messages.
/// Untrapped abnormal exits bypass the mailbox entirely (cooperative kill).
#[derive(Clone, Debug)]
pub enum SysMsg {
  /// A linked process exited (delivered only when `trap_exit` is set,
  /// except for `Normal` reasons which are always message-delivered).
  Exit {
    /// The process that exited.
    from: Pid,
    /// Its exit reason.
    reason: ExitReason,
  },
  /// A monitored process exited. Delivered exactly once per monitor.
  Down {
    /// The monitor this notification belongs to.
    monitor: MonitorRef,
    /// The process that exited.
    pid: Pid,
    /// Its exit reason.
    reason: ExitReason,
  },
}

/// Out-of-band message passed to [`crate::process::Process::handle_info`].
///
/// `#[non_exhaustive]`: a `match` needs a wildcard arm, so later notifications are additive.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum Info<I> {
  /// A linked process exited and this process traps exits.
  Exit {
    /// The process that exited.
    from: Pid,
    /// Its exit reason.
    reason: ExitReason,
  },
  /// A monitored process exited.
  Down {
    /// The monitor this notification belongs to.
    monitor: MonitorRef,
    /// The process that exited.
    pid: Pid,
    /// Its exit reason.
    reason: ExitReason,
  },
  /// A plain user message (BEAM's bare `send`).
  User(I),
  /// The idle timeout set with [`crate::ProcessCtx::set_idle_timeout`] passed with no message
  /// (gen_server's `timeout`). Delivered once per idle period; it never enters the mailbox, so
  /// a full mailbox cannot hold it back.
  Timeout,
}

impl<I> From<SysMsg> for Info<I> {
  fn from(sys: SysMsg) -> Self {
    match sys {
      SysMsg::Exit { from, reason } => Info::Exit { from, reason },
      SysMsg::Down {
        monitor,
        pid,
        reason,
      } => Info::Down {
        monitor,
        pid,
        reason,
      },
    }
  }
}

/// Typed mailbox envelope for a process implementing behavior `P`.
pub enum ProcMsg<P: Process> {
  /// Synchronous request; the caller awaits the [`Deferred`] reply.
  Call(P::Call, Deferred<P::Reply, CallError>),
  /// Fire-and-forget request.
  Cast(P::Cast),
  /// Out-of-band message (exit signals, monitor downs, user info).
  Info(Info<P::Info>),
}

/// Failure modes of [`crate::addr::Addr::call`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
  /// No reply arrived within the deadline. The request may still be
  /// processed — at-most-once semantics, exactly like `GenServer.call`.
  Timeout,
  /// The target exited before replying.
  Down(ExitReason),
  /// The target's mailbox is closed (process dead or dying).
  MailboxClosed,
  /// The target's mailbox is full; the request was not enqueued.
  MailboxFull,
}

impl fmt::Display for CallError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CallError::Timeout => f.write_str("call timed out"),
      CallError::Down(r) => write!(f, "callee exited: {r}"),
      CallError::MailboxClosed => f.write_str("mailbox closed"),
      CallError::MailboxFull => f.write_str("mailbox full"),
    }
  }
}

impl std::error::Error for CallError {}

/// Failure modes of sending to a mailbox.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
  /// The target process is dead or dying: its mailbox is closed.
  Rejected,
  /// The target's mailbox holds its capacity of user messages; the message was dropped.
  Full,
}

impl fmt::Display for SendError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      SendError::Rejected => f.write_str("message rejected: mailbox closed"),
      SendError::Full => f.write_str("message rejected: mailbox full"),
    }
  }
}

impl std::error::Error for SendError {}

/// Failure modes of a selective receive ([`crate::ProcessCtx::receive`]).
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveError {
  /// No matching message arrived within the deadline.
  Timeout,
  /// The mailbox is closed: the process is being killed.
  Closed,
  /// A mailbox's capacity of messages was scanned without a match.
  ScanLimit,
  /// The context belongs to a process of another behavior than the one named.
  BehaviorMismatch,
}

impl fmt::Display for ReceiveError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ReceiveError::Timeout => f.write_str("receive timed out"),
      ReceiveError::Closed => f.write_str("mailbox closed"),
      ReceiveError::ScanLimit => {
        f.write_str("receive scanned the mailbox capacity without a match")
      }
      ReceiveError::BehaviorMismatch => f.write_str("receive on a process of another behavior"),
    }
  }
}

impl std::error::Error for ReceiveError {}

/// Failure modes of starting a child process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartError {
  /// The start closure refused to start the child.
  Failed(String),
}

impl fmt::Display for StartError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      StartError::Failed(msg) => write!(f, "start failed: {msg}"),
    }
  }
}

impl std::error::Error for StartError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn error_messages_name_their_failure() {
    assert_eq!(ReceiveError::Timeout.to_string(), "receive timed out");
    assert_eq!(ReceiveError::Closed.to_string(), "mailbox closed");
    assert_eq!(
      ReceiveError::ScanLimit.to_string(),
      "receive scanned the mailbox capacity without a match"
    );
    assert_eq!(
      ReceiveError::BehaviorMismatch.to_string(),
      "receive on a process of another behavior"
    );
    assert_eq!(
      SendError::Rejected.to_string(),
      "message rejected: mailbox closed"
    );
    assert_eq!(
      SendError::Full.to_string(),
      "message rejected: mailbox full"
    );
    assert_eq!(
      StartError::Failed("nope".into()).to_string(),
      "start failed: nope"
    );
    assert_eq!(
      CallError::Down(ExitReason::Killed).to_string(),
      "callee exited: killed"
    );
    assert_eq!(CallError::MailboxFull.to_string(), "mailbox full");
  }
}
