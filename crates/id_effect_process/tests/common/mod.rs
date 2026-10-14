//! Shared fixtures for the deterministic contract tests.
//!
//! No test here sleeps: progress is observed through channels fed by
//! monitors / behaviour callbacks, and "fully finalized" is observed by
//! joining the process thread ([`JoinNode`]).

#![allow(dead_code)] // each test binary uses a different subset

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use id_effect::runtime::Never;
use id_effect::{Effect, succeed};
use id_effect_process::{
  ChildSpec, ExitReason, Info, Job, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode,
  SpinClock,
};

/// Generous upper bound for any single observation (never a sleep).
pub const T: Duration = Duration::from_secs(10);

/// Thread-per-process node that remembers each process thread so a test
/// can join it — after the join, the process is fully finalized (links
/// and monitors have fanned out).
pub struct JoinNode {
  pub node: ProcessNode,
  handles: Arc<Mutex<Vec<Option<JoinHandle<()>>>>>,
}

impl JoinNode {
  pub fn new(name: &str) -> Self {
    let handles: Arc<Mutex<Vec<Option<JoinHandle<()>>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&handles);
    let node = ProcessNode::with_spawner(
      NodeName::new(name),
      Arc::new(SpinClock),
      Arc::new(move |job: Job| {
        let handle = std::thread::spawn(job);
        sink.lock().unwrap().push(Some(handle));
      }),
    );
    Self { node, handles }
  }

  /// Join the `index`-th spawned process (spawn order; only meaningful
  /// when the test thread is the only spawner).
  pub fn join(&self, index: usize) {
    let handle = self.handles.lock().unwrap()[index]
      .take()
      .expect("process thread already joined");
    handle.join().expect("process thread panicked");
  }
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
  rx.recv_timeout(T).expect("exit observed before deadline")
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
    Effect::new(move |_| match msg {
      RecCall::Log => Ok((self.log.clone(), Next::Continue(self))),
      RecCall::Crash => Err("recorder crash".to_string()),
      RecCall::Stop => Ok((self.log.clone(), Next::Stop(ExitReason::Normal, self))),
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
  rx.recv_timeout(T)
    .expect("child start observed before deadline")
}
