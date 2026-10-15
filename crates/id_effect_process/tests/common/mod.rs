//! Shared fixtures for the deterministic contract tests.
//!
//! Every node runs on a [`TestExecutor`]: nothing runs until the test
//! drives it, time is virtual, and nothing sleeps. [`JoinNode::new`] makes
//! its executor the current thread's test executor, so the free helpers
//! ([`run`], [`settle`], [`recv`], [`await_exit`], [`advance`]) drive it
//! without threading it through every call.

#![allow(dead_code)] // each test binary uses a different subset

use std::cell::RefCell;
use std::pin::pin;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use id_effect::{Effect, HNil, TestExecutor, fail, succeed, suspend};
use id_effect::{Exit, Never};
use id_effect_process::{
  ChildSpec, ExitReason, Info, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode,
};

/// Call timeout used everywhere (virtual time: it only elapses through [`advance`]).
pub const T: Duration = Duration::from_secs(10);

thread_local! {
  static EXEC: RefCell<Option<TestExecutor>> = const { RefCell::new(None) };
}

fn exec() -> TestExecutor {
  EXEC.with(|e| e.borrow().clone().expect("no JoinNode on this test thread"))
}

/// A node on a fresh [`TestExecutor`] (made this thread's current one).
pub struct JoinNode {
  pub node: ProcessNode,
  pub exec: TestExecutor,
}

impl JoinNode {
  pub fn new(name: &str) -> Self {
    let exec = TestExecutor::new();
    EXEC.with(|e| *e.borrow_mut() = Some(exec.clone()));
    let node = ProcessNode::on_executor(NodeName::new(name), Arc::new(exec.clone()));
    Self { node, exec }
  }

  /// Wait until the `index`-th spawned process (spawn order on this node,
  /// pids start at 1) is fully finalized: its links and monitors have
  /// fanned out.
  pub fn join(&self, index: usize) {
    let pid = Pid::from_parts(self.node.name().clone(), index as u64 + 1, 0);
    let _ = run(self.node.await_exit::<Never, HNil>(&pid));
  }
}

/// Run every queued task until the executor stalls.
pub fn settle() {
  exec().run_until_stalled();
}

/// Move virtual time forward by `d` (firing due timers).
pub fn advance(d: Duration) {
  exec().advance(d);
}

/// Run `eff` on the current test executor until it stalls; it must have
/// finished by then. Typed failures come back as `Err`, defects panic.
pub fn run<A, E>(eff: Effect<A, E, HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + std::fmt::Debug + 'static,
{
  let exec = exec();
  let handle = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  let mut join = pin!(handle.join_future());
  match join.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
    Poll::Ready(Exit::Success(a)) => Ok(a),
    Poll::Ready(Exit::Failure(id_effect::Cause::Fail(e))) => Err(e),
    Poll::Ready(Exit::Failure(cause)) => panic!("effect died: {cause:?}"),
    Poll::Pending => panic!("effect still pending after the executor stalled"),
  }
}

/// Settle, then take the next message without waiting.
pub fn recv<T>(rx: &Receiver<T>) -> Result<T, TryRecvError> {
  settle();
  rx.try_recv()
}

/// Channel receiving the exit reason of `pid` exactly when its monitors fire.
pub fn watch_exit(node: &ProcessNode, pid: &Pid) -> Receiver<ExitReason> {
  let (tx, rx) = mpsc::channel();
  node.monitor_with_sink(
    pid,
    Box::new(move |_monitor, _pid, reason| {
      let _ = tx.send(reason);
    }),
  );
  rx
}

pub fn await_exit(rx: &Receiver<ExitReason>) -> ExitReason {
  recv(rx).expect("exit observed once the executor stalled")
}

// ── Recorder: logs casts/infos, crashes or stops on request ─────────

pub struct Recorder {
  log: Vec<i64>,
  on_terminate: Option<Sender<ExitReason>>,
}

pub enum RecCall {
  Log,
  Crash,
  Stop,
  /// Panics while the handler effect is being run.
  Panic,
}

impl Process for Recorder {
  type Args = Option<Sender<ExitReason>>;
  type Env = ();
  type Call = RecCall;
  type Reply = Vec<i64>;
  type Cast = i64;
  type Info = i64;
  type Error = String;

  fn init(on_terminate: Self::Args, _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    succeed(Recorder {
      log: Vec::new(),
      on_terminate,
    })
  }

  fn handle_call(
    self,
    msg: RecCall,
    _ctx: ProcessCtx,
  ) -> Effect<(Vec<i64>, Next<Self>), String, ()> {
    suspend(move || match msg {
      RecCall::Log => succeed((self.log.clone(), Next::Continue(self))),
      RecCall::Crash => fail("recorder crash".to_string()),
      RecCall::Stop => succeed((self.log.clone(), Next::Stop(ExitReason::Normal, self))),
      RecCall::Panic => panic!("recorder panic"),
    })
  }

  fn handle_cast(mut self, n: i64, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    self.log.push(n);
    succeed(Next::Continue(self))
  }

  fn handle_info(mut self, msg: Info<i64>, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    if let Info::User(n) = msg {
      self.log.push(n);
    }
    succeed(Next::Continue(self))
  }

  fn terminate(self, reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
    if let Some(tx) = &self.on_terminate {
      let _ = tx.send(reason.clone());
    }
    succeed(())
  }
}

// ── Probe: traps exits and forwards every Info to a channel ─────────

pub struct Probe {
  out: Sender<Info<()>>,
}

impl Process for Probe {
  type Args = Sender<Info<()>>;
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(out: Sender<Info<()>>, ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    ctx.trap_exit(true);
    succeed(Probe { out })
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }

  fn handle_info(self, msg: Info<()>, _ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    let _ = self.out.send(msg);
    succeed(Next::Continue(self))
  }
}

// ── Worker: reports starts/stops for supervision tests ─────────────

#[derive(Clone)]
pub struct WorkerArgs {
  pub id: &'static str,
  pub started: Sender<(&'static str, Pid)>,
  pub stopped: Option<Sender<&'static str>>,
  /// Trap exits (ignores trappable `Shutdown`, forcing kill escalation).
  pub trap: bool,
}

pub struct Worker {
  args: WorkerArgs,
}

pub enum WorkerCall {
  Ping,
  StopNormal,
  /// Panics in the handler body, before any effect is built.
  Panic,
}

impl Process for Worker {
  type Args = WorkerArgs;
  type Env = ();
  type Call = WorkerCall;
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(args: WorkerArgs, ctx: ProcessCtx) -> Effect<Self, String, ()> {
    ctx.trap_exit(args.trap);
    let _ = args.started.send((args.id, ctx.pid().clone()));
    succeed(Worker { args })
  }

  fn handle_call(self, msg: WorkerCall, _ctx: ProcessCtx) -> Effect<((), Next<Self>), String, ()> {
    match msg {
      WorkerCall::Ping => succeed(((), Next::Continue(self))),
      WorkerCall::StopNormal => succeed(((), Next::Stop(ExitReason::Normal, self))),
      WorkerCall::Panic => panic!("worker panic"),
    }
  }

  fn terminate(self, _reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
    if let Some(tx) = &self.args.stopped {
      let _ = tx.send(self.args.id);
    }
    succeed(())
  }
}

pub fn worker_spec(args: WorkerArgs) -> ChildSpec {
  let id = args.id;
  ChildSpec::worker::<Worker, _>(id, move || (args.clone(), ()))
}

/// Like [`worker_spec`], but every incarnation registers its typed address
/// under the child id (names are dropped automatically on exit).
pub fn registered_worker_spec(args: WorkerArgs) -> ChildSpec {
  let id = args.id;
  ChildSpec::from_start(
    id,
    Arc::new(move |node, supervisor| {
      let addr = node.spawn_linked_to::<Worker>(supervisor, args.clone(), ());
      node.register(id, &addr);
      Ok(addr.pid().clone())
    }),
  )
}

pub fn worker_args(id: &'static str, started: &Sender<(&'static str, Pid)>) -> WorkerArgs {
  WorkerArgs {
    id,
    started: started.clone(),
    stopped: None,
    trap: false,
  }
}

/// Next start notification (`(child id, pid)`).
pub fn next_start(rx: &Receiver<(&'static str, Pid)>) -> (&'static str, Pid) {
  recv(rx).expect("child start observed once the executor stalled")
}
