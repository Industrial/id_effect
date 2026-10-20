//! The [`Process`] behavior trait, [`ProcessCtx`], and the process loop.
//!
//! A process is a fiber draining a typed mailbox. Behavior callbacks are
//! effect-native: each returns `Effect<_, Error, Env>` and state threads
//! through by value (Rust ownership replaces BEAM's per-process heap).
//!
//! ## Loop semantics
//!
//! 1. `init` runs first; failure exits the process with `Error(..)`.
//! 2. Each mailbox message dispatches to `handle_call` / `handle_cast` /
//!    `handle_info`. Handler failure exits the process abnormally
//!    (**without** running `terminate` — the state was consumed by the
//!    failing handler; this is the honest Rust-ownership variant of
//!    BEAM's crash semantics). A panic in `init`, a handler, or `terminate`
//!    reaches the loop as a [`Defect`](id_effect::Defect) (`Cause::Die`, the
//!    kernel's panic boundary) and is treated the same way, with reason
//!    `ExitReason::Error("panic: <message>")`.
//! 3. A cooperative kill (link cascade, `exit/2`, supervisor shutdown)
//!    closes the mailbox; the loop notices at the next message boundary
//!    and runs `terminate` with the intact state.
//! 4. On every exit path — including an interrupt of the process fiber —
//!    the mailbox is closed (later sends fail with `SendError::Rejected`)
//!    and the process is finalized exactly once: exit signals fan out to
//!    links, monitors fire, registered names are dropped and pending
//!    timers are cancelled.
//! 5. With an idle timeout set ([`ProcessCtx::set_idle_timeout`]), a wait
//!    for the next message that lasts the timeout dispatches
//!    [`Info::Timeout`] instead; the next wait without a message in between
//!    has no deadline (one timeout per idle period).
//! 6. [`Next::Hibernate`] runs [`Process::hibernate`], releases the
//!    mailbox buffer and waits for the next message like `Continue`.

use std::any::Any;
use std::fmt::Debug;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use id_effect::{Cause, Exit, Never};
use id_effect::{Effect, fail, succeed, suspend, sync};

use crate::addr::Addr;
use crate::cell::ProcessCell;
use crate::exit::ExitReason;
use crate::mailbox::Mailbox;
use crate::msg::{Info, ProcMsg, ReceiveError};
use crate::node::ProcessNode;
use crate::pid::{MonitorRef, Pid};
use crate::timer::{self, TimerRef};

/// Continue-or-stop decision returned by behavior callbacks.
///
/// `#[non_exhaustive]`: a `match` needs a wildcard arm, so later decisions are additive.
#[non_exhaustive]
pub enum Next<P> {
  /// Keep processing messages with the new state.
  Continue(P),
  /// Stop with `reason`; `terminate` runs with the final state.
  Stop(ExitReason, P),
  /// Keep the new state but shed memory while idle (BEAM's `hibernate`): the loop runs
  /// [`Process::hibernate`], releases the mailbox buffer
  /// ([`Mailbox::shrink_to_fit`]) and resumes on the next message.
  Hibernate(P),
}

/// An OTP-style behavior: GenServer-shaped, effect-native.
///
/// All callbacks run inside the process fiber under the environment `Env`
/// (capabilities, clients, config), which is provided once at spawn.
pub trait Process: Sized + Send + 'static {
  /// Arguments passed to [`Process::init`] (crosses the spawn boundary).
  type Args: Send + 'static;
  /// Effect environment for all callbacks.
  type Env: Send + Sync + 'static;
  /// Synchronous request payload.
  type Call: Send + 'static;
  /// Reply to a [`Process::handle_call`].
  type Reply: Clone + Send + Sync + 'static;
  /// Fire-and-forget payload.
  type Cast: Send + 'static;
  /// User out-of-band message payload.
  type Info: Send + 'static;
  /// Behavior error; rendered into [`ExitReason::Error`] on crash.
  type Error: Debug + Clone + Send + Sync + 'static;

  /// Build the initial state. Failure exits the process immediately.
  fn init(args: Self::Args, ctx: ProcessCtx) -> Effect<Self, Self::Error, Self::Env>;

  /// Handle a synchronous request; the reply completes the caller's wait.
  #[allow(clippy::type_complexity)]
  fn handle_call(
    self,
    msg: Self::Call,
    ctx: ProcessCtx,
  ) -> Effect<(Self::Reply, Next<Self>), Self::Error, Self::Env>;

  /// Handle a fire-and-forget message. Default: ignore and continue.
  fn handle_cast(
    self,
    msg: Self::Cast,
    ctx: ProcessCtx,
  ) -> Effect<Next<Self>, Self::Error, Self::Env> {
    let _ = (msg, ctx);
    succeed(Next::Continue(self))
  }

  /// Handle exit signals, monitor downs, and user info messages.
  /// Default: ignore and continue.
  fn handle_info(
    self,
    msg: Info<Self::Info>,
    ctx: ProcessCtx,
  ) -> Effect<Next<Self>, Self::Error, Self::Env> {
    let _ = (msg, ctx);
    succeed(Next::Continue(self))
  }

  /// Hibernation hook, run when a callback returns [`Next::Hibernate`] and before the mailbox
  /// buffer is released: compact the state here (drop caches, shrink collections). A failure
  /// crashes the process like a handler failure. Default: keep the state as is.
  fn hibernate(self, ctx: ProcessCtx) -> Effect<Self, Self::Error, Self::Env> {
    let _ = ctx;
    succeed(self)
  }

  /// Graceful-cleanup hook. Runs on [`Next::Stop`] and on cooperative
  /// kills; **not** on handler crashes (state already consumed).
  fn terminate(self, reason: &ExitReason, ctx: ProcessCtx) -> Effect<(), Never, Self::Env> {
    let _ = (reason, ctx);
    succeed(())
  }
}

/// Handle to the current process, passed to every behavior callback.
///
/// Cloneable and cheap; also usable from outside the callbacks (e.g. by
/// custom start logic) as long as the process is alive.
#[derive(Clone)]
pub struct ProcessCtx {
  pid: Pid,
  node: ProcessNode,
  /// The process's own `Mailbox<ProcMsg<P>>`, type-erased (for [`ProcessCtx::receive`]).
  mailbox: Option<Arc<dyn Any + Send + Sync>>,
}

impl ProcessCtx {
  pub(crate) fn new(pid: Pid, node: ProcessNode) -> Self {
    Self {
      pid,
      node,
      mailbox: None,
    }
  }

  pub(crate) fn bind_mailbox<P: Process>(mut self, mailbox: Mailbox<ProcMsg<P>>) -> Self {
    self.mailbox = Some(Arc::new(mailbox));
    self
  }

  /// Selective receive (BEAM's `receive ... after`): take the first message in this process's
  /// mailbox that `matcher` accepts, waiting up to `timeout` (on the executor clock) for one to
  /// arrive. Every skipped message stays queued, in arrival order, for the normal loop.
  ///
  /// `P` is this process's behavior (`Self` inside its callbacks). The matched message is
  /// returned as is: a matched [`ProcMsg::Call`] must be answered through its reply. The scan
  /// covers at most the mailbox capacity ([`ReceiveError::ScanLimit`] past it); a kill fails it
  /// with [`ReceiveError::Closed`] and the deadline with [`ReceiveError::Timeout`]. `matcher`
  /// runs under the mailbox lock: it may only inspect the message.
  pub fn receive<P: Process, R: Send + Sync + 'static>(
    &self,
    matcher: impl FnMut(&ProcMsg<P>) -> bool + Send + 'static,
    timeout: Duration,
  ) -> Effect<ProcMsg<P>, ReceiveError, R> {
    match self
      .mailbox
      .as_ref()
      .and_then(|m| m.downcast_ref::<Mailbox<ProcMsg<P>>>())
    {
      Some(mailbox) => mailbox
        .take_matching(Box::new(matcher))
        .timeout_fail(timeout, || ReceiveError::Timeout),
      None => fail(ReceiveError::BehaviorMismatch),
    }
  }

  /// Deliver `msg` to this process's `handle_info` (as [`Info::User`]) after `after` on the
  /// executor clock (BEAM's `erlang:send_after/3`). `P` is this process's behavior (`Self`
  /// inside its callbacks); for another behavior the timer is inert and never delivers.
  ///
  /// The timer dies with the process. The message counts against the mailbox bound when it
  /// fires: a full mailbox drops it (D17). Cancel with [`cancel_timer`](Self::cancel_timer).
  pub fn send_after<P: Process>(&self, msg: P::Info, after: Duration) -> TimerRef {
    match self
      .mailbox
      .as_ref()
      .and_then(|m| m.downcast_ref::<Mailbox<ProcMsg<P>>>())
    {
      Some(mailbox) => {
        let mailbox = mailbox.clone();
        timer::start(
          &self.node,
          &self.pid,
          after,
          Box::new(move || {
            let _ = mailbox.offer_now(ProcMsg::Info(Info::User(msg)));
          }),
        )
      }
      None => TimerRef::inert(),
    }
  }

  /// Cancel a timer (BEAM's `erlang:cancel_timer/1`). `true` means its message will never be
  /// delivered; `false` that it already fired or was cancelled.
  pub fn cancel_timer(&self, timer: TimerRef) -> bool {
    timer.cancel()
  }

  /// Deliver [`Info::Timeout`] when this process waits `after` (executor clock) for its next
  /// message: once per idle period, re-armed by the next message. Replaces an earlier setting;
  /// applies from the next wait.
  pub fn set_idle_timeout(&self, after: Duration) {
    if let Some(cell) = self.node.table().lookup(&self.pid) {
      cell.set_idle_timeout(Some(after));
    }
  }

  /// Remove the idle timeout set with [`set_idle_timeout`](Self::set_idle_timeout).
  pub fn clear_idle_timeout(&self) {
    if let Some(cell) = self.node.table().lookup(&self.pid) {
      cell.set_idle_timeout(None);
    }
  }

  /// This process's pid.
  pub fn pid(&self) -> &Pid {
    &self.pid
  }

  /// The node this process runs on.
  pub fn node(&self) -> &ProcessNode {
    &self.node
  }

  /// Convert link exits into [`Info::Exit`] messages instead of dying
  /// (BEAM's `process_flag(trap_exit, true)`).
  pub fn trap_exit(&self, trap: bool) {
    if let Some(cell) = self.node.table().lookup(&self.pid) {
      cell.set_trap_exit(trap);
    }
  }

  /// Bidirectionally link this process with `peer`.
  pub fn link(&self, peer: &Pid) {
    self.node.link(&self.pid, peer);
  }

  /// Remove a link previously created with [`ProcessCtx::link`].
  pub fn unlink(&self, peer: &Pid) {
    self.node.unlink(&self.pid, peer);
  }

  /// Monitor `peer`; a [`Info::Down`] message arrives in this process's
  /// mailbox exactly once when `peer` exits.
  pub fn monitor(&self, peer: &Pid) -> MonitorRef {
    self.node.monitor_into(&self.pid, peer)
  }

  /// Remove a monitor before it fires.
  pub fn demonitor(&self, peer: &Pid, monitor: MonitorRef) {
    self.node.demonitor(peer, monitor);
  }

  /// Spawn a child process linked to this one.
  pub fn spawn_link<P: Process>(&self, args: P::Args, env: P::Env) -> Addr<P> {
    self.node.spawn_linked_to::<P>(&self.pid, args, env)
  }

  /// Spawn an unlinked process on the same node.
  pub fn spawn<P: Process>(&self, args: P::Args, env: P::Env) -> Addr<P> {
    self.node.spawn::<P>(args, env)
  }

  /// Whether this process has been asked to die (cooperative kill).
  pub fn is_killed(&self) -> bool {
    self
      .node
      .table()
      .lookup(&self.pid)
      .map(|cell| cell.recorded_kill().is_some())
      .unwrap_or(true)
  }
}

/// The exit reason for a callback that ended with `cause` instead of a value.
fn crash_reason<E: Debug>(cause: &Cause<E>) -> ExitReason {
  match cause {
    Cause::Fail(e) => ExitReason::error(e),
    Cause::Die(defect) => ExitReason::Error(format!("panic: {}", defect.message())),
    Cause::Interrupt(_) => ExitReason::Interrupted,
    Cause::Both(l, _) | Cause::Then(l, _) => crash_reason(l),
  }
}

/// Runs a behavior callback (built lazily, so a panic while building it is
/// caught too) and turns any failure — typed error, defect or interrupt —
/// into the crash reason.
fn guarded<A, E, R>(
  callback: impl FnOnce() -> Effect<A, E, R> + Send + 'static,
) -> Effect<Result<A, ExitReason>, Never, R>
where
  A: Send + 'static,
  E: Debug + Send + 'static,
  R: Send + Sync + 'static,
{
  suspend(callback).exit::<Never>().map(|exit| match exit {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(crash_reason(&cause)),
  })
}

/// What the loop carries from message to message besides the state.
struct Looper<P: Process> {
  ctx: ProcessCtx,
  mailbox: Mailbox<ProcMsg<P>>,
  cell: Option<Arc<ProcessCell>>,
  /// Whether the next wait honours the idle timeout (`false` right after an `Info::Timeout`).
  idle_armed: bool,
}

impl<P: Process> Looper<P> {
  fn killed(&self) -> Option<ExitReason> {
    self.cell.as_ref().and_then(|c| c.recorded_kill())
  }
}

type Step<P> = Effect<ExitReason, Never, <P as Process>::Env>;

/// Run `terminate`; a panic in it replaces the stop reason.
fn terminate<P: Process>(state: P, reason: ExitReason, ctx: ProcessCtx) -> Step<P> {
  let arg = reason.clone();
  guarded(move || P::terminate(state, &arg, ctx)).map(move |r| match r {
    Ok(()) => reason,
    Err(crash) => crash,
  })
}

/// Next message boundary: kill checkpoint, then take and dispatch.
fn step<P: Process>(state: P, lp: Looper<P>) -> Step<P> {
  // A cooperative kill may have arrived while a handler ran or while
  // messages were still buffered.
  if let Some(r) = lp.killed() {
    return terminate(state, r, lp.ctx);
  }
  let idle = match &lp.cell {
    Some(cell) if lp.idle_armed => cell.idle_timeout(),
    _ => None,
  };
  let next = match idle {
    Some(after) => {
      let deadline = lp.ctx.node().runtime().executor().sleep(after);
      lp.mailbox.take_or_idle(deadline)
    }
    None => lp.mailbox.take().map(Some),
  };
  next.exit::<Never>().flat_map(move |exit| match exit {
    Exit::Success(Some(msg)) => dispatch(
      state,
      msg,
      Looper {
        idle_armed: true,
        ..lp
      },
    ),
    Exit::Success(None) => dispatch(
      state,
      ProcMsg::Info(Info::Timeout),
      Looper {
        idle_armed: false,
        ..lp
      },
    ),
    // Closed: either a kill (reason recorded) or a graceful stop.
    Exit::Failure(Cause::Fail(_)) => {
      let r = lp.killed().unwrap_or(ExitReason::Normal);
      terminate(state, r, lp.ctx)
    }
    Exit::Failure(cause) => terminate(state, crash_reason(&cause), lp.ctx),
  })
}

fn proceed<P: Process>(outcome: Result<Next<P>, ExitReason>, lp: Looper<P>) -> Step<P> {
  match outcome {
    Ok(Next::Continue(s)) => step(s, lp),
    Ok(Next::Stop(r, s)) => terminate(s, r, lp.ctx),
    Ok(Next::Hibernate(s)) => {
      let ctx = lp.ctx.clone();
      guarded(move || P::hibernate(s, ctx)).flat_map(move |r| match r {
        Ok(s) => {
          lp.mailbox.shrink_to_fit();
          step(s, lp)
        }
        Err(crash) => succeed(crash),
      })
    }
    Err(crash) => succeed(crash),
  }
}

fn dispatch<P: Process>(state: P, msg: ProcMsg<P>, lp: Looper<P>) -> Step<P> {
  let ctx = lp.ctx.clone();
  match msg {
    ProcMsg::Call(m, reply) => guarded(move || P::handle_call(state, m, ctx)).flat_map(move |r| {
      let outcome = r.map(|(rep, next)| {
        reply.try_succeed(rep);
        next
      });
      proceed(outcome, lp)
    }),
    ProcMsg::Cast(m) => {
      guarded(move || P::handle_cast(state, m, ctx)).flat_map(move |r| proceed(r, lp))
    }
    ProcMsg::Info(i) => {
      guarded(move || P::handle_info(state, i, ctx)).flat_map(move |r| proceed(r, lp))
    }
  }
}

/// Build the complete process-loop effect for behavior `P`.
///
/// The effect never fails (`E = Never`); every failure mode is converted
/// into an [`ExitReason`] and fanned out by finalization, which runs as a
/// finalizer (`ensuring`) so an interrupted process is finalized too.
pub(crate) fn process_loop<P: Process>(
  args: P::Args,
  ctx: ProcessCtx,
  mailbox: Mailbox<ProcMsg<P>>,
) -> Effect<(), Never, P::Env> {
  let pid = ctx.pid().clone();
  let node = ctx.node().clone();
  let lp = Looper::<P> {
    ctx: ctx.clone(),
    mailbox: mailbox.clone(),
    cell: node.table().lookup(&pid),
    idle_armed: true,
  };
  let outcome: Arc<Mutex<Option<ExitReason>>> = Arc::new(Mutex::new(None));
  let slot = Arc::clone(&outcome);

  guarded(move || P::init(args, ctx))
    .flat_map(move |r| match r {
      Ok(state) => step(state, lp),
      Err(crash) => succeed(crash),
    })
    .map(move |reason| {
      *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
    })
    .ensuring(sync(move || {
      // Close on every exit path (graceful stops, init failures and panics
      // do not go through `kill`), so later sends to this pid are rejected
      // instead of silently queueing into a dead mailbox.
      mailbox.close();
      let reason = outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .unwrap_or(ExitReason::Interrupted);
      node.finalize_process(&pid, reason);
    }))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::pin::pin;
  use std::sync::mpsc;
  use std::task::{Context, Poll, Waker};

  use id_effect::{HNil, TestExecutor, fail};

  use crate::pid::NodeName;

  /// Behavior that drives [`ProcessCtx`] operations from inside its own callbacks.
  struct Probe {
    downs: i64,
  }

  enum ProbeCall {
    Downs,
    LinkThenUnlink(Pid),
    MonitorThenDemonitor(Pid),
    SpawnSibling,
    IsKilled,
    Monitor(Pid),
  }

  impl Process for Probe {
    type Args = ();
    type Env = HNil;
    type Call = ProbeCall;
    type Reply = i64;
    /// `true` makes the handler fail.
    type Cast = bool;
    /// `true` makes the handler fail.
    type Info = bool;
    type Error = String;

    fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, String, HNil> {
      succeed(Probe { downs: 0 })
    }

    fn handle_call(
      self,
      msg: ProbeCall,
      ctx: ProcessCtx,
    ) -> Effect<(i64, Next<Self>), String, HNil> {
      sync(move || {
        let reply = match msg {
          ProbeCall::Downs => self.downs,
          ProbeCall::LinkThenUnlink(peer) => {
            ctx.link(&peer);
            ctx.unlink(&peer);
            0
          }
          ProbeCall::MonitorThenDemonitor(peer) => {
            let monitor = ctx.monitor(&peer);
            ctx.demonitor(&peer, monitor);
            0
          }
          ProbeCall::SpawnSibling => {
            let sibling = ctx.spawn::<Probe>((), HNil);
            i64::from(sibling.pid() != ctx.pid() && sibling.is_alive())
          }
          ProbeCall::IsKilled => i64::from(ctx.is_killed()),
          ProbeCall::Monitor(peer) => {
            let _ = ctx.monitor(&peer);
            0
          }
        };
        (reply, Next::Continue(self))
      })
    }

    fn handle_cast(self, fail_it: bool, _ctx: ProcessCtx) -> Effect<Next<Self>, String, HNil> {
      if fail_it {
        fail("cast boom".to_string())
      } else {
        succeed(Next::Continue(self))
      }
    }

    fn handle_info(
      mut self,
      msg: Info<bool>,
      _ctx: ProcessCtx,
    ) -> Effect<Next<Self>, String, HNil> {
      match msg {
        Info::User(true) => fail("info boom".to_string()),
        Info::Down { .. } => {
          self.downs += 1;
          succeed(Next::Continue(self))
        }
        _ => succeed(Next::Continue(self)),
      }
    }
  }

  struct Harness {
    exec: TestExecutor,
    node: ProcessNode,
  }

  fn harness() -> Harness {
    let exec = TestExecutor::new();
    let node = ProcessNode::on_executor(NodeName::new("probe@local"), Arc::new(exec.clone()));
    Harness { exec, node }
  }

  impl Harness {
    /// Runs `eff` on the node's executor until everything stalls; it must be done by then.
    fn run<A: Send + 'static, E: Send + 'static>(&self, eff: Effect<A, E, HNil>) -> Exit<A, E> {
      let handle = self.node.runtime().fork(eff);
      self.exec.run_until_stalled();
      let mut join = pin!(handle.join_future());
      match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(exit) => exit,
        Poll::Pending => panic!("effect still pending after the executor stalled"),
      }
    }

    fn call(&self, addr: &Addr<Probe>, msg: ProbeCall) -> i64 {
      match self.run(addr.call(msg, std::time::Duration::from_secs(10))) {
        Exit::Success(n) => n,
        Exit::Failure(c) => panic!("call failed: {c:?}"),
      }
    }

    /// Exit reason of `pid`, observed through a raw monitor sink.
    fn exit_watch(&self, pid: &Pid) -> mpsc::Receiver<ExitReason> {
      let (tx, rx) = mpsc::channel();
      let tx = Mutex::new(tx);
      self.node.monitor_with_sink(
        pid,
        Box::new(move |_m, _p, reason| {
          let _ = tx.lock().expect("tx").send(reason);
        }),
      );
      rx
    }
  }

  #[test]
  fn unlinked_peer_exit_does_not_propagate() {
    let h = harness();
    let probe = h.node.spawn::<Probe>((), HNil);
    let peer = h.node.spawn::<Probe>((), HNil);
    h.call(&probe, ProbeCall::LinkThenUnlink(peer.pid().clone()));
    let peer_exit = h.exit_watch(peer.pid());
    h.node
      .exit(peer.pid(), ExitReason::Error("peer died".into()));
    h.exec.run_until_stalled();
    assert_eq!(
      peer_exit.try_recv().expect("peer exit"),
      ExitReason::Error("peer died".into())
    );
    assert_eq!(h.call(&probe, ProbeCall::IsKilled), 0);
  }

  #[test]
  fn demonitored_peer_exit_sends_no_down() {
    let h = harness();
    let probe = h.node.spawn::<Probe>((), HNil);
    let peer = h.node.spawn::<Probe>((), HNil);
    h.call(&probe, ProbeCall::MonitorThenDemonitor(peer.pid().clone()));
    let peer_exit = h.exit_watch(peer.pid());
    h.node.exit(peer.pid(), ExitReason::Shutdown);
    h.exec.run_until_stalled();
    peer_exit.try_recv().expect("peer exit");
    assert_eq!(h.call(&probe, ProbeCall::Downs), 0);
  }

  #[test]
  fn ctx_spawn_starts_an_unlinked_sibling() {
    let h = harness();
    let probe = h.node.spawn::<Probe>((), HNil);
    assert_eq!(h.call(&probe, ProbeCall::SpawnSibling), 1);
    assert_eq!(h.call(&probe, ProbeCall::IsKilled), 0);
  }

  #[test]
  fn cast_handler_failure_exits_with_its_error() {
    let h = harness();
    let probe = h.node.spawn::<Probe>((), HNil);
    let exit = h.exit_watch(probe.pid());
    assert!(matches!(h.run(probe.cast(true)), Exit::Success(())));
    match exit.try_recv().expect("exit") {
      ExitReason::Error(msg) => assert!(msg.contains("cast boom"), "{msg}"),
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  #[test]
  fn info_handler_failure_exits_with_its_error() {
    let h = harness();
    let probe = h.node.spawn::<Probe>((), HNil);
    let exit = h.exit_watch(probe.pid());
    assert!(matches!(h.run(probe.send(true)), Exit::Success(())));
    match exit.try_recv().expect("exit") {
      ExitReason::Error(msg) => assert!(msg.contains("info boom"), "{msg}"),
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  #[test]
  fn ctx_for_a_dead_pid_reports_killed() {
    let h = harness();
    let ctx = ProcessCtx::new(
      Pid::from_parts(NodeName::new("probe@local"), 999, 1),
      h.node,
    );
    ctx.trap_exit(true); // no cell: a no-op rather than a panic
    assert!(ctx.is_killed());
  }

  #[test]
  fn crash_reason_renders_defects_as_panics() {
    assert_eq!(
      crash_reason(&Cause::<String>::Die(id_effect::Defect::new("boom"))),
      ExitReason::Error("panic: boom".into())
    );
    assert_eq!(
      crash_reason(&Cause::<String>::Fail("e".into())),
      ExitReason::Error("\"e\"".into())
    );
    let fiber = id_effect::FiberId::ROOT;
    assert_eq!(
      crash_reason(&Cause::<String>::Interrupt(fiber)),
      ExitReason::Interrupted
    );
    assert_eq!(
      crash_reason(&Cause::both(
        Cause::<String>::Interrupt(fiber),
        Cause::Fail("x".into())
      )),
      ExitReason::Interrupted
    );
  }

  #[test]
  fn probe_ignores_harmless_casts_and_infos_and_counts_downs() {
    let h = harness();
    let probe = h.node.spawn::<Probe>((), HNil);
    let peer = h.node.spawn::<Probe>((), HNil);
    assert!(matches!(h.run(probe.cast(false)), Exit::Success(())));
    assert!(matches!(h.run(probe.send(false)), Exit::Success(())));
    h.call(&probe, ProbeCall::Monitor(peer.pid().clone()));
    h.node.exit(peer.pid(), ExitReason::Killed);
    h.exec.run_until_stalled();
    assert_eq!(h.call(&probe, ProbeCall::Downs), 1);
  }

  /// Behavior that keeps every default callback (`handle_cast`, `handle_info`, `hibernate`,
  /// `terminate`) and exercises the context operations aimed at other behaviors.
  struct Plain;

  enum PlainCall {
    Hibernate,
    Stop,
    ReceiveForeign,
    SendAfterForeign,
    SpawnLinked,
  }

  #[derive(Clone, Debug, PartialEq)]
  enum PlainReply {
    Done,
    Flag(bool),
    Child(Pid),
  }

  impl Process for Plain {
    type Args = ();
    type Env = HNil;
    type Call = PlainCall;
    type Reply = PlainReply;
    type Cast = ();
    type Info = ();
    type Error = String;

    fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, String, HNil> {
      succeed(Plain)
    }

    fn handle_call(
      self,
      msg: PlainCall,
      ctx: ProcessCtx,
    ) -> Effect<(PlainReply, Next<Self>), String, HNil> {
      match msg {
        PlainCall::Hibernate => succeed((PlainReply::Done, Next::Hibernate(self))),
        PlainCall::Stop => succeed((PlainReply::Done, Next::Stop(ExitReason::Normal, self))),
        PlainCall::ReceiveForeign => ctx
          .receive::<Probe, HNil>(|_| true, Duration::from_secs(1))
          .exit::<String>()
          .map(move |exit| {
            let mismatch = matches!(
              exit,
              Exit::Failure(Cause::Fail(ReceiveError::BehaviorMismatch))
            );
            (PlainReply::Flag(mismatch), Next::Continue(self))
          }),
        PlainCall::SendAfterForeign => {
          let timer = ctx.send_after::<Probe>(false, Duration::from_secs(1));
          succeed((PlainReply::Flag(timer.is_pending()), Next::Continue(self)))
        }
        PlainCall::SpawnLinked => {
          let child = ctx.spawn_link::<Plain>((), HNil);
          succeed((PlainReply::Child(child.pid().clone()), Next::Continue(self)))
        }
      }
    }
  }

  fn plain_call(h: &Harness, addr: &Addr<Plain>, msg: PlainCall) -> PlainReply {
    match h.run(addr.call(msg, Duration::from_secs(10))) {
      Exit::Success(r) => r,
      Exit::Failure(c) => panic!("call failed: {c:?}"),
    }
  }

  #[test]
  fn default_callbacks_keep_the_process_running() {
    let h = harness();
    let plain = h.node.spawn::<Plain>((), HNil);
    assert!(matches!(h.run(plain.cast(())), Exit::Success(())));
    assert!(matches!(h.run(plain.send(())), Exit::Success(())));
    assert_eq!(
      plain_call(&h, &plain, PlainCall::Hibernate),
      PlainReply::Done
    );
    // Still serving after the default hibernate hook.
    assert_eq!(
      plain_call(&h, &plain, PlainCall::ReceiveForeign),
      PlainReply::Flag(true)
    );
    let exit = h.exit_watch(plain.pid());
    assert_eq!(plain_call(&h, &plain, PlainCall::Stop), PlainReply::Done);
    h.exec.run_until_stalled();
    assert_eq!(exit.try_recv().expect("exit"), ExitReason::Normal);
  }

  #[test]
  fn send_after_for_another_behavior_is_inert() {
    let h = harness();
    let plain = h.node.spawn::<Plain>((), HNil);
    assert_eq!(
      plain_call(&h, &plain, PlainCall::SendAfterForeign),
      PlainReply::Flag(false)
    );
  }

  #[test]
  fn spawn_link_ties_the_child_to_its_parent() {
    let h = harness();
    let parent = h.node.spawn::<Plain>((), HNil);
    let PlainReply::Child(child) = plain_call(&h, &parent, PlainCall::SpawnLinked) else {
      panic!("expected a child pid");
    };
    let child_exit = h.exit_watch(&child);
    h.node
      .exit(parent.pid(), ExitReason::Error("parent died".into()));
    h.exec.run_until_stalled();
    assert_eq!(
      child_exit.try_recv().expect("child exit"),
      ExitReason::Error("parent died".into())
    );
  }

  /// Behavior whose `hibernate` fails and whose `terminate` panics.
  struct Fragile;

  impl Process for Fragile {
    type Args = ();
    type Env = HNil;
    type Call = bool;
    type Reply = ();
    type Cast = ();
    type Info = ();
    type Error = String;

    fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, String, HNil> {
      succeed(Fragile)
    }

    /// `true` hibernates, `false` stops.
    fn handle_call(
      self,
      hibernate: bool,
      _ctx: ProcessCtx,
    ) -> Effect<((), Next<Self>), String, HNil> {
      if hibernate {
        succeed(((), Next::Hibernate(self)))
      } else {
        succeed(((), Next::Stop(ExitReason::Shutdown, self)))
      }
    }

    fn hibernate(self, _ctx: ProcessCtx) -> Effect<Self, String, HNil> {
      fail("hibernate boom".to_string())
    }

    fn terminate(self, _reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, HNil> {
      panic!("terminate boom")
    }
  }

  #[test]
  fn failing_hibernate_crashes_the_process() {
    let h = harness();
    let fragile = h.node.spawn::<Fragile>((), HNil);
    let exit = h.exit_watch(fragile.pid());
    assert!(matches!(
      h.run(fragile.call(true, Duration::from_secs(10))),
      Exit::Success(())
    ));
    h.exec.run_until_stalled();
    match exit.try_recv().expect("exit") {
      ExitReason::Error(msg) => assert!(msg.contains("hibernate boom"), "{msg}"),
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  #[test]
  fn panicking_terminate_replaces_the_stop_reason() {
    let h = harness();
    let fragile = h.node.spawn::<Fragile>((), HNil);
    let exit = h.exit_watch(fragile.pid());
    assert!(matches!(
      h.run(fragile.call(false, Duration::from_secs(10))),
      Exit::Success(())
    ));
    h.exec.run_until_stalled();
    assert_eq!(
      exit.try_recv().expect("exit"),
      ExitReason::Error("panic: terminate boom".into())
    );
  }
}
