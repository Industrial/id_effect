//! The [`Process`] behavior trait, [`ProcessCtx`], and the process loop.
//!
//! A process is a supervised fiber draining a typed mailbox. Behavior
//! callbacks are effect-native: each returns `Effect<_, Error, Env>` and
//! state threads through by value (Rust ownership replaces BEAM's
//! per-process heap).
//!
//! ## Loop semantics
//!
//! 1. `init` runs first; failure exits the process with `Error(..)`.
//! 2. Each mailbox message dispatches to `handle_call` / `handle_cast` /
//!    `handle_info`. Handler failure exits the process abnormally
//!    (**without** running `terminate` — the state was consumed by the
//!    failing handler; this is the honest Rust-ownership variant of
//!    BEAM's crash semantics). A panic in `init`, a handler, or `terminate`
//!    is caught and treated the same way, with reason
//!    `ExitReason::Error("panic: <message>")`.
//! 3. A cooperative kill (link cascade, `exit/2`, supervisor shutdown)
//!    closes the mailbox; the loop notices at the next message boundary
//!    and runs `terminate` with the intact state.
//! 4. On every exit path the mailbox is closed (later sends fail with
//!    `SendError::Rejected`) and the process is finalized exactly once: exit
//!    signals fan out to links, monitors fire, and registered names are
//!    dropped.

use std::any::Any;
use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};

use id_effect::runtime::Never;
use id_effect::{Effect, Queue, box_future, succeed};

use crate::addr::Addr;
use crate::exit::ExitReason;
use crate::msg::{Info, ProcMsg};
use crate::node::ProcessNode;
use crate::pid::{MonitorRef, Pid};

/// Continue-or-stop decision returned by behavior callbacks.
pub enum Next<P> {
  /// Keep processing messages with the new state.
  Continue(P),
  /// Stop with `reason`; `terminate` runs with the final state.
  Stop(ExitReason, P),
}

/// An OTP-style behavior: GenServer-shaped, effect-native.
///
/// All callbacks run inside the process fiber with exclusive access to the
/// environment `Env` (capabilities, clients, config).
pub trait Process: Sized + 'static {
  /// Arguments passed to [`Process::init`] (crosses the spawn boundary).
  type Args: Send + 'static;
  /// Effect environment for all callbacks.
  type Env: Send + 'static;
  /// Synchronous request payload.
  type Call: Send + 'static;
  /// Reply to a [`Process::handle_call`].
  type Reply: Clone + Send + Sync + 'static;
  /// Fire-and-forget payload.
  type Cast: Send + 'static;
  /// User out-of-band message payload.
  type Info: Send + 'static;
  /// Behavior error; rendered into [`ExitReason::Error`] on crash.
  type Error: std::fmt::Debug + Clone + Send + Sync + 'static;

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
}

impl ProcessCtx {
  pub(crate) fn new(pid: Pid, node: ProcessNode) -> Self {
    Self { pid, node }
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

/// Polls the inner future, converting a panic during any poll into `Err`.
struct CatchUnwind<F>(F);

impl<F: Future + Unpin> Future for CatchUnwind<F> {
  type Output = Result<F::Output, Box<dyn Any + Send>>;

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    let inner = &mut self.0;
    match panic::catch_unwind(AssertUnwindSafe(|| Pin::new(inner).poll(cx))) {
      Ok(Poll::Ready(out)) => Poll::Ready(Ok(out)),
      Ok(Poll::Pending) => Poll::Pending,
      Err(payload) => Poll::Ready(Err(payload)),
    }
  }
}

/// Best-effort text of a panic payload (`&str` / `String` payloads).
fn panic_message(payload: &(dyn Any + Send)) -> String {
  if let Some(s) = payload.downcast_ref::<&str>() {
    (*s).to_string()
  } else if let Some(s) = payload.downcast_ref::<String>() {
    s.clone()
  } else {
    "non-string panic payload".to_string()
  }
}

/// Build the complete process-loop effect for behavior `P`.
///
/// The effect never fails (`E = Never`); every failure mode is converted
/// into an [`ExitReason`] and fanned out by finalization.
pub(crate) fn process_loop<P: Process>(
  args: P::Args,
  ctx: ProcessCtx,
  mailbox: Queue<ProcMsg<P>>,
) -> Effect<(), Never, P::Env> {
  Effect::new_async(move |env| {
    box_future(async move {
      let pid = ctx.pid().clone();
      let node = ctx.node().clone();
      let cell = node.table().lookup(&pid);

      let recorded = |fallback: ExitReason| -> ExitReason {
        cell
          .as_ref()
          .and_then(|c| c.recorded_kill())
          .unwrap_or(fallback)
      };

      let body = async {
        match P::init(args, ctx.clone()).run(env).await {
          Err(e) => ExitReason::error(e),
          Ok(mut state) => {
            let exit: ExitReason;
            loop {
              // Kill checkpoint: a cooperative kill may have arrived while a
              // handler ran or while messages were still buffered.
              if let Some(c) = cell.as_ref()
                && let Some(r) = c.recorded_kill()
              {
                let _ = P::terminate(state, &r, ctx.clone()).run(env).await;
                exit = r;
                break;
              }
              let msg = match mailbox.take().run(&mut ()).await {
                Ok(m) => m,
                Err(_) => {
                  // Mailbox closed: either a kill (reason recorded) or all
                  // senders dropped (treated as a graceful stop).
                  let r = recorded(ExitReason::Normal);
                  let _ = P::terminate(state, &r, ctx.clone()).run(env).await;
                  exit = r;
                  break;
                }
              };
              let next = match msg {
                ProcMsg::Call(m, reply) => {
                  match P::handle_call(state, m, ctx.clone()).run(env).await {
                    Ok((rep, next)) => {
                      reply.try_succeed(rep);
                      next
                    }
                    Err(e) => {
                      exit = ExitReason::error(e);
                      break;
                    }
                  }
                }
                ProcMsg::Cast(m) => match P::handle_cast(state, m, ctx.clone()).run(env).await {
                  Ok(next) => next,
                  Err(e) => {
                    exit = ExitReason::error(e);
                    break;
                  }
                },
                ProcMsg::Info(i) => match P::handle_info(state, i, ctx.clone()).run(env).await {
                  Ok(next) => next,
                  Err(e) => {
                    exit = ExitReason::error(e);
                    break;
                  }
                },
              };
              match next {
                Next::Continue(s) => state = s,
                Next::Stop(r, s) => {
                  let _ = P::terminate(s, &r, ctx.clone()).run(env).await;
                  exit = r;
                  break;
                }
              }
            }
            exit
          }
        }
      };
      // A panic in `init`, a handler, or `terminate` is a crash like any
      // other: the state is gone, and the process still exits through the
      // same close-and-finalize path so links, monitors, pending calls, and
      // supervisors all observe it.
      let reason = match CatchUnwind(Box::pin(body)).await {
        Ok(reason) => reason,
        Err(payload) => ExitReason::Error(format!("panic: {}", panic_message(&*payload))),
      };

      // Close the mailbox on every exit path (graceful stops, init failures
      // and panics do not go through `kill`), so later sends to this pid are
      // rejected instead of silently queueing into a dead mailbox.
      let _ = mailbox.shutdown().run(&mut ()).await;
      node.finalize_process(&pid, reason);
      Ok(())
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::mpsc;
  use std::time::Duration;

  use id_effect::run_blocking;

  use crate::pid::NodeName;

  const WAIT: Duration = Duration::from_secs(10);

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
  }

  impl Process for Probe {
    type Args = ();
    type Env = ();
    type Call = ProbeCall;
    type Reply = i64;
    /// `true` makes the handler fail.
    type Cast = bool;
    /// `true` makes the handler fail.
    type Info = bool;
    type Error = String;

    fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, String, ()> {
      succeed(Probe { downs: 0 })
    }

    fn handle_call(self, msg: ProbeCall, ctx: ProcessCtx) -> Effect<(i64, Next<Self>), String, ()> {
      Effect::new(move |_| {
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
            let sibling = ctx.spawn::<Probe>((), ());
            i64::from(sibling.pid() != ctx.pid() && sibling.is_alive())
          }
          ProbeCall::IsKilled => i64::from(ctx.is_killed()),
        };
        Ok((reply, Next::Continue(self)))
      })
    }

    fn handle_cast(self, fail: bool, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
      Effect::new(move |_| {
        if fail {
          Err("cast boom".to_string())
        } else {
          Ok(Next::Continue(self))
        }
      })
    }

    fn handle_info(mut self, msg: Info<bool>, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
      Effect::new(move |_| match msg {
        Info::User(true) => Err("info boom".to_string()),
        Info::Down { .. } => {
          self.downs += 1;
          Ok(Next::Continue(self))
        }
        _ => Ok(Next::Continue(self)),
      })
    }
  }

  fn node() -> ProcessNode {
    ProcessNode::threads(NodeName::new("probe@local"))
  }

  fn call(addr: &Addr<Probe>, msg: ProbeCall) -> i64 {
    run_blocking(addr.call(msg, WAIT), ()).expect("call")
  }

  /// Exit reason of `pid`, observed through a raw monitor sink (no polling).
  fn exit_watch(node: &ProcessNode, pid: &Pid) -> mpsc::Receiver<ExitReason> {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    node.monitor_with_sink(
      pid,
      Box::new(move |_m, _p, reason| {
        let _ = tx.lock().expect("tx").send(reason);
      }),
    );
    rx
  }

  #[test]
  fn unlinked_peer_exit_does_not_propagate() {
    let node = node();
    let probe = node.spawn::<Probe>((), ());
    let peer = node.spawn::<Probe>((), ());
    call(&probe, ProbeCall::LinkThenUnlink(peer.pid().clone()));
    let peer_exit = exit_watch(&node, peer.pid());
    node.exit(peer.pid(), ExitReason::Error("peer died".into()));
    assert_eq!(
      peer_exit.recv_timeout(WAIT).expect("peer exit"),
      ExitReason::Error("peer died".into())
    );
    // Link exits fan out before monitors fire, so a surviving link would already have killed us.
    assert_eq!(call(&probe, ProbeCall::IsKilled), 0);
  }

  #[test]
  fn demonitored_peer_exit_sends_no_down() {
    let node = node();
    let probe = node.spawn::<Probe>((), ());
    let peer = node.spawn::<Probe>((), ());
    call(&probe, ProbeCall::MonitorThenDemonitor(peer.pid().clone()));
    let peer_exit = exit_watch(&node, peer.pid());
    node.exit(peer.pid(), ExitReason::Shutdown);
    peer_exit.recv_timeout(WAIT).expect("peer exit");
    assert_eq!(call(&probe, ProbeCall::Downs), 0);
  }

  #[test]
  fn ctx_spawn_starts_an_unlinked_sibling() {
    let node = node();
    let probe = node.spawn::<Probe>((), ());
    assert_eq!(call(&probe, ProbeCall::SpawnSibling), 1);
    assert_eq!(call(&probe, ProbeCall::IsKilled), 0);
  }

  #[test]
  fn cast_handler_failure_exits_with_its_error() {
    let node = node();
    let probe = node.spawn::<Probe>((), ());
    let exit = exit_watch(&node, probe.pid());
    run_blocking(probe.cast(true), ()).expect("cast");
    match exit.recv_timeout(WAIT).expect("exit") {
      ExitReason::Error(msg) => assert!(msg.contains("cast boom"), "{msg}"),
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  #[test]
  fn info_handler_failure_exits_with_its_error() {
    let node = node();
    let probe = node.spawn::<Probe>((), ());
    let exit = exit_watch(&node, probe.pid());
    run_blocking(probe.send(true), ()).expect("send");
    match exit.recv_timeout(WAIT).expect("exit") {
      ExitReason::Error(msg) => assert!(msg.contains("info boom"), "{msg}"),
      other => panic!("unexpected exit: {other:?}"),
    }
  }

  #[test]
  fn ctx_for_a_dead_pid_reports_killed() {
    let node = node();
    let ctx = ProcessCtx::new(Pid::from_parts(NodeName::new("probe@local"), 999, 1), node);
    ctx.trap_exit(true); // no cell: a no-op rather than a panic
    assert!(ctx.is_killed());
  }

  #[test]
  fn panic_message_renders_common_payloads() {
    assert_eq!(panic_message(&"static"), "static");
    assert_eq!(panic_message(&String::from("owned")), "owned");
    assert_eq!(panic_message(&42_u8), "non-string panic payload");
  }
}
