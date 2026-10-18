//! [`Addr`] — typed send/cast/call handle to a process mailbox.

use std::time::Duration;

use id_effect::Cause;
use id_effect::{Deferred, Effect, fail, succeed, suspend, sync};

use crate::exit::ExitReason;
use crate::mailbox::{Mailbox, OfferError};
use crate::msg::{CallError, Info, ProcMsg, SendError};
use crate::node::ProcessNode;
use crate::pid::Pid;
use crate::process::Process;

/// Typed handle to a process implementing behavior `P`.
///
/// Cloneable; sending to a dead process fails with [`SendError::Rejected`]
/// (send) or [`CallError`] (call), and sending to a full mailbox with
/// [`SendError::Full`] or [`CallError::MailboxFull`] — never panics. The
/// effects are generic over the environment `R`, so they bind inside any program.
pub struct Addr<P: Process> {
  pid: Pid,
  mailbox: Mailbox<ProcMsg<P>>,
  node: ProcessNode,
}

impl<P: Process> Clone for Addr<P> {
  fn clone(&self) -> Self {
    Self {
      pid: self.pid.clone(),
      mailbox: self.mailbox.clone(),
      node: self.node.clone(),
    }
  }
}

impl<P: Process> Addr<P> {
  pub(crate) fn new(pid: Pid, mailbox: Mailbox<ProcMsg<P>>, node: ProcessNode) -> Self {
    Self { pid, mailbox, node }
  }

  /// The pid this address points at.
  pub fn pid(&self) -> &Pid {
    &self.pid
  }

  /// Clone the typed mailbox (distribution layer installs wire gateways).
  pub fn clone_mailbox(&self) -> Mailbox<ProcMsg<P>> {
    self.mailbox.clone()
  }

  /// Whether the process is still alive on its node.
  pub fn is_alive(&self) -> bool {
    self.node.table().lookup(&self.pid).is_some()
  }

  /// Fire-and-forget request (`GenServer.cast`).
  pub fn cast<R: Send + Sync + 'static>(&self, msg: P::Cast) -> Effect<(), SendError, R> {
    self.offer(ProcMsg::Cast(msg))
  }

  /// Plain message delivered to `handle_info` (BEAM's bare `send`).
  pub fn send<R: Send + Sync + 'static>(&self, msg: P::Info) -> Effect<(), SendError, R> {
    self.offer(ProcMsg::Info(Info::User(msg)))
  }

  /// Non-effect variant of [`Addr::send`] for use from plain code
  /// (timers, transports). Returns `false` (and drops `msg`) if the mailbox
  /// is closed or full. Never waits.
  pub fn send_now(&self, msg: P::Info) -> bool {
    self.mailbox.offer_now(ProcMsg::Info(Info::User(msg)))
  }

  fn offer<R: Send + Sync + 'static>(&self, msg: ProcMsg<P>) -> Effect<(), SendError, R> {
    let mailbox = self.mailbox.clone();
    suspend(move || match mailbox.try_offer_now(msg) {
      Ok(()) => succeed(()),
      Err(OfferError::Full) => fail(SendError::Full),
      Err(OfferError::Closed) => fail(SendError::Rejected),
    })
  }

  /// Synchronous request with a deadline (`GenServer.call`).
  ///
  /// Implemented exactly as BEAM's convention: monitor + one-shot reply +
  /// timeout. If the callee exits before replying the call fails with
  /// [`CallError::Down`]; if the deadline passes first (on the calling
  /// fiber's executor clock) it fails with [`CallError::Timeout`] (the request may
  /// still execute — at-most-once).
  pub fn call<R: Send + Sync + 'static>(
    &self,
    msg: P::Call,
    timeout: Duration,
  ) -> Effect<P::Reply, CallError, R> {
    let mailbox = self.mailbox.clone();
    let node = self.node.clone();
    let pid = self.pid.clone();
    suspend(move || {
      let reply: Deferred<P::Reply, CallError> = Deferred::new();

      // The monitor completes the reply with `Down` if the callee dies
      // before replying (first completion wins).
      let down_reply = reply.clone();
      let monitor = node.monitor_with_sink(
        &pid,
        Box::new(move |_m, _p, reason: ExitReason| {
          down_reply.try_fail_cause(Cause::Fail(CallError::Down(reason)));
        }),
      );

      if let Err(refused) = mailbox.try_offer_now(ProcMsg::Call(msg, reply.clone())) {
        node.demonitor(&pid, monitor);
        return fail(match refused {
          OfferError::Full => CallError::MailboxFull,
          OfferError::Closed => CallError::MailboxClosed,
        });
      }

      reply
        .wait()
        .timeout_fail(timeout, || CallError::Timeout)
        .ensuring(sync(move || node.demonitor(&pid, monitor)))
    })
  }
}
