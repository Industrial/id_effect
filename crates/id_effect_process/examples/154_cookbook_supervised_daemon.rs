//! Ex 154 — Cookbook: a supervised daemon skeleton.
//!
//! - A `Worker` process handles jobs and crashes on a poison message.
//! - A `OneForOne` supervisor restarts it with fresh state (restart intensity: 3 in 5 s).
//! - On a shutdown signal the whole tree stops gracefully, and the worker's `terminate` hook
//!   flushes its state before it exits.
//!
//! The signal is simulated with a channel so the example is deterministic. In a real daemon,
//! feed that channel from `tokio::signal::ctrl_c()` or a SIGTERM handler (see the book recipe).
//!
//! Run: `cargo run -p id_effect_process --example 154_cookbook_supervised_daemon`

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use id_effect::Never;
use id_effect::next::{Effect, HNil, fail, succeed};
use id_effect_process::{
  ChildSpec, ExitReason, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode, ShutdownPolicy,
  StartFn, Strategy, SupervisorHandle, SupervisorSpec,
};

const TIMEOUT: Duration = Duration::from_secs(5);

/// A job for the worker.
enum Job {
  Work(u64),
  Poison,
}

/// A stateful worker: sums the jobs it has seen, writes the sum to `flushed` on shutdown.
struct Worker {
  total: u64,
  flushed: Arc<AtomicU64>,
}

impl Process for Worker {
  type Args = Arc<AtomicU64>;
  type Env = ();
  type Call = ();
  type Reply = u64;
  type Cast = Job;
  type Info = ();
  type Error = String;

  fn init(flushed: Arc<AtomicU64>, _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    succeed(Worker { total: 0, flushed })
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<(u64, Next<Self>), String, ()> {
    let total = self.total;
    succeed((total, Next::Continue(self)))
  }

  fn handle_cast(self, job: Job, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    match job {
      Job::Work(n) => succeed(Next::Continue(Worker {
        total: self.total + n,
        ..self
      })),
      // A failed handler crashes the process with `ExitReason::Error`; the supervisor sees it.
      Job::Poison => fail("poison message".to_string()),
    }
  }

  fn terminate(self, reason: &ExitReason, _ctx: ProcessCtx) -> Effect<(), Never, ()> {
    println!("worker: flushing total={} on {reason:?}", self.total);
    self.flushed.store(self.total, Ordering::SeqCst);
    succeed(())
  }
}

/// Child spec that (re)starts the worker linked to the supervisor and registers it by name, so
/// callers can always find the current incarnation with `whereis_addr`.
fn worker_child(flushed: Arc<AtomicU64>) -> ChildSpec {
  let start: StartFn = Arc::new(move |node: &ProcessNode, supervisor: &Pid| {
    let addr = node.spawn_linked_to::<Worker>(supervisor, Arc::clone(&flushed), ());
    node.register("worker", &addr);
    Ok(addr.pid().clone())
  });
  ChildSpec::from_start("worker", start)
}

/// Run `eff` on the default runtime from `main` (the blocking edge).
fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(eff: Effect<A, E, HNil>) -> A {
  eff.block_on().into_result().expect("effect succeeds")
}

/// Current pid of the supervised worker.
fn worker_pid(tree: &SupervisorHandle) -> Option<Pid> {
  run(tree.which_children(TIMEOUT))
    .into_iter()
    .find(|(id, _)| id == "worker")
    .and_then(|(_, pid)| pid)
}

fn main() {
  let node = ProcessNode::new(NodeName::new("daemon@local"));
  let flushed = Arc::new(AtomicU64::new(0));
  let tree = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(3, Duration::from_secs(5))
    .child(worker_child(Arc::clone(&flushed)))
    .start(&node);

  let first = worker_pid(&tree).expect("worker started");
  let worker = node.whereis_addr::<Worker>("worker").expect("registered");
  run(worker.cast(Job::Work(5)));

  // Crash it and wait (without sleeping) until it is finalized: its exit signal is then in the
  // supervisor's mailbox, so the next supervisor call is answered after the restart.
  run(worker.cast(Job::Poison));
  run(node.await_exit::<Never, HNil>(&first));
  let second = worker_pid(&tree).expect("supervisor restarted the worker");
  assert_ne!(first, second);
  println!("worker crashed and restarted: {first} -> {second}");

  // The new incarnation starts from fresh state.
  let worker = node
    .whereis_addr::<Worker>("worker")
    .expect("re-registered");
  for n in [1, 2, 3] {
    run(worker.cast(Job::Work(n)));
  }
  let total = run(worker.call((), TIMEOUT));
  assert_eq!(total, 6);

  // Wait for a shutdown signal, then stop the tree: children get a trappable `Shutdown`
  // (escalating to a kill after their shutdown timeout) and run `terminate`.
  let (signal_tx, signal_rx) = mpsc::channel::<&str>();
  signal_tx.send("SIGTERM").expect("signal");
  let signal = signal_rx.recv().expect("signal");
  println!("received {signal}, shutting down");
  run(tree.stop::<Never, HNil>(&node, ShutdownPolicy::Timeout(TIMEOUT)));

  assert_eq!(flushed.load(Ordering::SeqCst), 6, "terminate flushed state");
  assert!(!worker.is_alive());
  println!("154_cookbook_supervised_daemon ok");
}
