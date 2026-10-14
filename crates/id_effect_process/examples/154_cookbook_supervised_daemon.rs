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

use id_effect::runtime::Never;
use id_effect::{Effect, fail, run_blocking, succeed};
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

/// Current pid of the supervised worker.
fn worker_pid(tree: &SupervisorHandle) -> Option<Pid> {
  run_blocking(tree.which_children(TIMEOUT), ())
    .expect("supervisor answers")
    .into_iter()
    .find(|(id, _)| id == "worker")
    .and_then(|(_, pid)| pid)
}

fn main() {
  let node = ProcessNode::threads(NodeName::new("daemon@local"));
  let flushed = Arc::new(AtomicU64::new(0));
  let tree = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(3, Duration::from_secs(5))
    .child(worker_child(Arc::clone(&flushed)))
    .start(&node);

  let first = worker_pid(&tree).expect("worker started");
  let worker = node.whereis_addr::<Worker>("worker").expect("registered");
  run_blocking(worker.cast(Job::Work(5)), ()).expect("cast");

  // Crash it. The supervisor restarts it; wait (without sleeping) until the new pid shows up.
  run_blocking(worker.cast(Job::Poison), ()).expect("cast");
  let mut restarted = None;
  for _ in 0..100_000 {
    match worker_pid(&tree) {
      Some(pid) if pid != first => {
        restarted = Some(pid);
        break;
      }
      _ => std::thread::yield_now(),
    }
  }
  let second = restarted.expect("supervisor restarted the worker");
  println!("worker crashed and restarted: {first} -> {second}");

  // The new incarnation starts from fresh state.
  let worker = node
    .whereis_addr::<Worker>("worker")
    .expect("re-registered");
  for n in [1, 2, 3] {
    run_blocking(worker.cast(Job::Work(n)), ()).expect("cast");
  }
  let total = run_blocking(worker.call((), TIMEOUT), ()).expect("call");
  assert_eq!(total, 6);

  // Wait for a shutdown signal, then stop the tree: children get a trappable `Shutdown`
  // (escalating to a kill after their shutdown timeout) and run `terminate`.
  let (signal_tx, signal_rx) = mpsc::channel::<&str>();
  signal_tx.send("SIGTERM").expect("signal");
  let signal = signal_rx.recv().expect("signal");
  println!("received {signal}, shutting down");
  pollster::block_on(tree.stop(&node, ShutdownPolicy::Timeout(TIMEOUT)));

  assert_eq!(flushed.load(Ordering::SeqCst), 6, "terminate flushed state");
  assert!(!worker.is_alive());
  println!("154_cookbook_supervised_daemon ok");
}
