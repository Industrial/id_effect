//! Supervision-tree semantics: strategies, restart policies, intensity
//! escalation, shutdown protocol, and DynamicSupervisor.
//!
//! Every node runs on a `TestExecutor`: restarts, shutdown deadlines and
//! intensity windows are measured on its virtual clock.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{JoinNode, advance, run, settle};
use id_effect::Never;
use id_effect::{Effect, fail, succeed, suspend, sync};
use id_effect_process::{
  ChildSpec, DynamicSupervisor, DynamicSupervisorSpec, ExitReason, Next, Pid, Process, ProcessCtx,
  ProcessNode, RestartPolicy, ShutdownPolicy, StartChildFn, Strategy, SupervisorHandle,
  SupervisorSpec,
};

/// A node on this test's `TestExecutor` (virtual time, driven by `common`).
fn node() -> ProcessNode {
  JoinNode::new("suptest@local").node
}

/// Run the executor until it stalls, then evaluate `check`.
fn settled(check: impl FnOnce() -> bool) -> bool {
  settle();
  check()
}

const T: Duration = Duration::from_secs(5);

/// Worker that counts its own starts through a shared witness and crashes
/// on command.
struct Worker {
  witness: Arc<AtomicU32>,
}

#[allow(dead_code)] // crashes are induced via exit signals; calls kept for API completeness
enum WorkerCall {
  Ping,
  Crash,
  StopNormal,
}

impl Process for Worker {
  type Args = Arc<AtomicU32>;
  type Env = ();
  type Call = WorkerCall;
  type Reply = u32;
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(witness: Arc<AtomicU32>, _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    sync(move || {
      witness.fetch_add(1, Ordering::SeqCst);
      Worker { witness }
    })
  }

  fn handle_call(self, msg: WorkerCall, _ctx: ProcessCtx) -> Effect<(u32, Next<Self>), String, ()> {
    suspend(move || match msg {
      WorkerCall::Ping => {
        let starts = self.witness.load(Ordering::SeqCst);
        succeed((starts, Next::Continue(self)))
      }
      WorkerCall::Crash => fail("worker crash".to_string()),
      WorkerCall::StopNormal => succeed((0, Next::Stop(ExitReason::Normal, self))),
    })
  }
}

fn worker_spec(id: &str, witness: &Arc<AtomicU32>) -> ChildSpec {
  let witness = Arc::clone(witness);
  ChildSpec::worker::<Worker, _>(id, move || (Arc::clone(&witness), ()))
}

fn child_pid(handle: &SupervisorHandle, id: &str) -> Option<Pid> {
  run(handle.which_children(T))
    .ok()?
    .into_iter()
    .find(|(cid, _)| cid == id)?
    .1
}

fn crash_child(node: &ProcessNode, pid: &Pid) {
  // Reach into the child mailbox through a fresh typed addr via registry-
  // free path: send an exit signal that the (non-trapping) worker cannot
  // trap. Using a distinct reason keeps crash-vs-shutdown observable.
  node.exit(pid, ExitReason::Error("induced crash".to_string()));
}

#[test]
fn one_for_one_restarts_only_the_crashed_child() {
  let node = node();
  let w1 = Arc::new(AtomicU32::new(0));
  let w2 = Arc::new(AtomicU32::new(0));
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(10, Duration::from_secs(60))
    .child(worker_spec("a", &w1))
    .child(worker_spec("b", &w2))
    .start(&node);

  assert!(settled(
    || w1.load(Ordering::SeqCst) == 1 && w2.load(Ordering::SeqCst) == 1
  ));

  let a_pid = child_pid(&handle, "a").expect("child a running");
  crash_child(&node, &a_pid);

  assert!(
    settled(|| w1.load(Ordering::SeqCst) == 2),
    "crashed child must restart"
  );
  settle();
  assert_eq!(w2.load(Ordering::SeqCst), 1, "sibling must not restart");

  let a_pid2 = child_pid(&handle, "a").expect("child a running again");
  assert_ne!(a_pid, a_pid2, "restart produces a fresh pid");
}

#[test]
fn one_for_all_restarts_every_child() {
  let node = node();
  let w1 = Arc::new(AtomicU32::new(0));
  let w2 = Arc::new(AtomicU32::new(0));
  let handle = SupervisorSpec::new(Strategy::OneForAll)
    .intensity(10, Duration::from_secs(60))
    .child(worker_spec("a", &w1))
    .child(worker_spec("b", &w2))
    .start(&node);

  assert!(settled(
    || w1.load(Ordering::SeqCst) == 1 && w2.load(Ordering::SeqCst) == 1
  ));
  let a_pid = child_pid(&handle, "a").expect("child a running");
  crash_child(&node, &a_pid);

  assert!(settled(
    || w1.load(Ordering::SeqCst) == 2 && w2.load(Ordering::SeqCst) == 2
  ));
}

#[test]
fn rest_for_one_restarts_crashed_child_and_later_siblings_only() {
  let node = node();
  let w1 = Arc::new(AtomicU32::new(0));
  let w2 = Arc::new(AtomicU32::new(0));
  let w3 = Arc::new(AtomicU32::new(0));
  let handle = SupervisorSpec::new(Strategy::RestForOne)
    .intensity(10, Duration::from_secs(60))
    .child(worker_spec("a", &w1))
    .child(worker_spec("b", &w2))
    .child(worker_spec("c", &w3))
    .start(&node);

  assert!(settled(|| w3.load(Ordering::SeqCst) == 1));
  let b_pid = child_pid(&handle, "b").expect("child b running");
  crash_child(&node, &b_pid);

  assert!(settled(
    || w2.load(Ordering::SeqCst) == 2 && w3.load(Ordering::SeqCst) == 2
  ));
  settle();
  assert_eq!(
    w1.load(Ordering::SeqCst),
    1,
    "earlier sibling must not restart"
  );
}

#[test]
fn transient_child_is_not_restarted_after_graceful_stop() {
  let node = node();
  let w = Arc::new(AtomicU32::new(0));
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(worker_spec("a", &w).with_restart(RestartPolicy::Transient))
    .start(&node);

  assert!(settled(|| w.load(Ordering::SeqCst) == 1));
  let a_pid = child_pid(&handle, "a").expect("running");
  node.exit(&a_pid, ExitReason::Shutdown);

  settle();
  assert_eq!(
    w.load(Ordering::SeqCst),
    1,
    "transient + graceful: no restart"
  );
}

#[test]
fn intensity_exceeded_stops_supervisor_and_escalates() {
  let node = node();
  let w = Arc::new(AtomicU32::new(0));
  // 1 restart per minute allowed; the second crash breaches intensity.
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(1, Duration::from_secs(60))
    .child(worker_spec("a", &w))
    .start(&node);

  assert!(settled(|| w.load(Ordering::SeqCst) == 1));

  for _ in 0..2 {
    if let Some(pid) = child_pid(&handle, "a") {
      crash_child(&node, &pid);
      settle();
    }
  }

  assert!(
    settled(|| node.table().lookup(handle.pid()).is_none()),
    "supervisor must stop once intensity is exceeded"
  );
}

#[test]
fn intensity_window_slides_on_virtual_time() {
  let node = node();
  let w = Arc::new(AtomicU32::new(0));
  // 1 restart per 10 s: two crashes 10 s apart (virtual) are both restarted.
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(1, Duration::from_secs(10))
    .child(worker_spec("a", &w))
    .start(&node);
  assert!(settled(|| w.load(Ordering::SeqCst) == 1));

  crash_child(&node, &child_pid(&handle, "a").expect("running"));
  assert!(settled(|| w.load(Ordering::SeqCst) == 2), "first restart");

  advance(Duration::from_secs(11));
  crash_child(&node, &child_pid(&handle, "a").expect("running again"));
  assert!(
    settled(|| w.load(Ordering::SeqCst) == 3),
    "the first restart left the window"
  );
  assert!(node.table().lookup(handle.pid()).is_some(), "within budget");

  // A third crash inside the same 10 s breaches the budget.
  crash_child(
    &node,
    &child_pid(&handle, "a").expect("running a third time"),
  );
  assert!(settled(|| node.table().lookup(handle.pid()).is_none()));
  assert_eq!(w.load(Ordering::SeqCst), 3);
}

#[test]
fn shutdown_timeout_escalates_to_kill_on_virtual_time() {
  let node = node();
  let w = Arc::new(AtomicU32::new(0));
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::worker::<Stubborn, _>("s", {
      let w = Arc::clone(&w);
      move || (Arc::clone(&w), ())
    }))
    .start(&node);
  assert!(settled(|| w.load(Ordering::SeqCst) == 1));
  let child = child_pid(&handle, "s").expect("running");

  let stop = handle
    .stop::<Never, id_effect::HNil>(&node, ShutdownPolicy::Infinity)
    .fork_root(Arc::clone(node.runtime().executor()));
  settle();
  // The root asks its child to shut down with the default 5 s deadline;
  // the child traps it, so only the deadline's kill ends it.
  assert!(node.table().lookup(&child).is_some(), "trapped shutdown");
  advance(Duration::from_secs(4));
  assert!(
    node.table().lookup(&child).is_some(),
    "deadline not reached"
  );
  advance(Duration::from_secs(1));
  assert!(
    node.table().lookup(&child).is_none(),
    "killed at virtual 5 s"
  );
  assert!(stop.is_done());
}

/// Worker that traps exits, so a trappable `Shutdown` does not stop it.
struct Stubborn;

impl Process for Stubborn {
  type Args = Arc<AtomicU32>;
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(witness: Arc<AtomicU32>, ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    sync(move || {
      ctx.trap_exit(true);
      witness.fetch_add(1, Ordering::SeqCst);
      Stubborn
    })
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }
}

#[test]
fn nested_supervisor_shutdown_stops_grandchildren() {
  let node = node();
  let w = Arc::new(AtomicU32::new(0));
  let inner_witness = Arc::clone(&w);

  let root = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::supervisor("inner", move || {
      SupervisorSpec::new(Strategy::OneForOne).child(worker_spec("leaf", &inner_witness))
    }))
    .start(&node);

  assert!(settled(|| w.load(Ordering::SeqCst) == 1));
  let before = node.table().len();
  assert!(before >= 3, "root + inner + leaf alive, got {before}");

  run::<(), Never>(root.stop(&node, ShutdownPolicy::Timeout(Duration::from_secs(2))))
    .expect("tree stopped");

  assert!(
    settled(|| node.table().is_empty()),
    "whole tree must be gone, {} processes remain",
    node.table().len()
  );
}

#[test]
fn start_child_and_terminate_child_manage_dynamic_members() {
  let node = node();
  let w = Arc::new(AtomicU32::new(0));
  let handle = SupervisorSpec::new(Strategy::OneForOne).start(&node);

  assert_eq!(run(handle.count_children(T)), Ok(0));

  let started = run(handle.start_child(worker_spec("a", &w), T))
    .expect("call ok")
    .expect("start ok");
  assert!(node.table().lookup(&started).is_some());
  assert_eq!(run(handle.count_children(T)), Ok(1));

  assert_eq!(run(handle.terminate_child("a", T)), Ok(true));
  assert!(settled(|| node.table().lookup(&started).is_none()));
  assert_eq!(run(handle.count_children(T)), Ok(0));
}

// ── DynamicSupervisor ──────────────────────────────────────────

struct Echo;

impl Process for Echo {
  type Args = ();
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(_: (), _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Echo)
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }
}

#[test]
fn dynamic_supervisor_starts_restarts_and_counts_children() {
  let node = node();
  let sup = DynamicSupervisor::start(
    &node,
    DynamicSupervisorSpec {
      restart: RestartPolicy::Permanent,
      ..DynamicSupervisorSpec::default()
    },
  );

  let start: StartChildFn =
    Arc::new(|node, sup_pid| Ok(node.spawn_linked_to::<Echo>(sup_pid, (), ()).pid().clone()));

  let pid = run(sup.start_dyn_child(Arc::clone(&start), T))
    .expect("call ok")
    .expect("start ok");
  assert_eq!(run(sup.count_dyn_children(T)), Ok(1));

  // Crash the child: Permanent policy must replace it.
  node.exit(&pid, ExitReason::Error("boom".to_string()));
  assert!(settled(|| {
    node.table().lookup(&pid).is_none() && run(sup.count_dyn_children(T)) == Ok(1)
  }));
}
