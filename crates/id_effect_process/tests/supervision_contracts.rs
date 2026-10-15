//! Deterministic supervision contracts: strategies, restart policies,
//! intensity escalation, shutdown ordering and escalation, dynamic children.
//!
//! Restarts are observed through start notifications sent from each
//! worker's `init`; "the supervisor has processed the exit" is established
//! by waiting on the child's monitor (links fan out before monitors), after
//! which any supervisor `call` is ordered behind that exit. Every node runs
//! on a `TestExecutor`, so deadlines and intensity windows are virtual time.

mod common;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use common::{
  JoinNode, Probe, T, Worker, WorkerArgs, WorkerCall, advance, await_exit, next_start, recv,
  registered_worker_spec, run, settle, watch_exit, worker_args, worker_spec,
};
use id_effect::Never;
use id_effect::next::HNil;
use id_effect_process::{
  ChildSpec, DynamicSupervisor, DynamicSupervisorSpec, ExitReason, Info, Pid, ProcessNode,
  RestartPolicy, ShutdownPolicy, StartChildFn, StartError, Strategy, SupError, SupervisorHandle,
  SupervisorProcess, SupervisorSpec,
};

fn children(handle: &SupervisorHandle) -> Vec<(String, Option<Pid>)> {
  run(handle.which_children(T)).expect("which_children")
}

fn pid_of(handle: &SupervisorHandle, id: &str) -> Option<Pid> {
  children(handle).into_iter().find(|(c, _)| c == id)?.1
}

/// Crash `pid` with an abnormal reason and wait until it is finalized.
fn crash(node: &ProcessNode, pid: &Pid) {
  let exits = watch_exit(node, pid);
  node.exit(pid, ExitReason::Error("induced".to_string()));
  await_exit(&exits);
}

fn three_children(
  strategy: Strategy,
) -> (
  JoinNode,
  SupervisorHandle,
  mpsc::Receiver<(&'static str, Pid)>,
) {
  let jn = JoinNode::new("sup@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(strategy)
    .intensity(10, Duration::from_secs(60))
    .child(worker_spec(worker_args("a", &tx)))
    .child(worker_spec(worker_args("b", &tx)))
    .child(worker_spec(worker_args("c", &tx)))
    .start(&jn.node);
  assert_eq!(starts(&rx, 3), ["a", "b", "c"]);
  (jn, handle, rx)
}

/// The next `n` start notifications, sorted (each child runs `init` in its
/// own fiber, so notification order need not be the spawn order).
fn starts(rx: &mpsc::Receiver<(&'static str, Pid)>, n: usize) -> Vec<&'static str> {
  let mut ids: Vec<_> = (0..n).map(|_| next_start(rx).0).collect();
  ids.sort_unstable();
  ids
}

// ── Strategies ────────────────────────────────────────────────────

#[test]
fn one_for_one_replaces_only_the_crashed_child() {
  let (jn, handle, rx) = three_children(Strategy::OneForOne);
  let before = children(&handle);
  crash(&jn.node, before[1].1.as_ref().unwrap());
  assert_eq!(next_start(&rx).0, "b");
  let after = children(&handle);
  assert_eq!(after[0].1, before[0].1, "a untouched");
  assert_ne!(after[1].1, before[1].1, "b has a fresh pid");
  assert_eq!(after[2].1, before[2].1, "c untouched");
  assert!(rx.try_recv().is_err(), "no other restarts");
}

#[test]
fn one_for_all_replaces_every_child() {
  let (jn, handle, rx) = three_children(Strategy::OneForAll);
  let before = children(&handle);
  crash(&jn.node, before[1].1.as_ref().unwrap());
  assert_eq!(starts(&rx, 3), ["a", "b", "c"]);
  let after = children(&handle);
  for (old, new) in before.iter().zip(&after) {
    assert_ne!(old.1, new.1, "{} restarted", old.0);
  }
}

#[test]
fn rest_for_one_replaces_the_crashed_child_and_later_siblings() {
  let (jn, handle, rx) = three_children(Strategy::RestForOne);
  let before = children(&handle);
  crash(&jn.node, before[1].1.as_ref().unwrap());
  assert_eq!(starts(&rx, 2), ["b", "c"]);
  let after = children(&handle);
  assert_eq!(after[0].1, before[0].1, "earlier sibling untouched");
  assert_ne!(after[2].1, before[2].1);
}

// ── Restart policies ──────────────────────────────────────────────

#[test]
fn temporary_child_is_never_restarted() {
  let jn = JoinNode::new("temp@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(worker_spec(worker_args("t", &tx)).with_restart(RestartPolicy::Temporary))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  crash(&jn.node, &pid);
  assert_eq!(children(&handle), vec![("t".to_string(), None)]);
  assert!(rx.try_recv().is_err());
}

#[test]
fn transient_child_is_restarted_after_an_abnormal_exit() {
  let jn = JoinNode::new("transient@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(worker_spec(worker_args("t", &tx)).with_restart(RestartPolicy::Transient))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  crash(&jn.node, &pid);
  let (_, again) = next_start(&rx);
  assert_ne!(pid, again);
  assert_eq!(pid_of(&handle, "t"), Some(again));
}

#[test]
fn transient_child_is_not_restarted_after_a_normal_exit() {
  let jn = JoinNode::new("transient-normal@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(registered_worker_spec(worker_args("t", &tx)).with_restart(RestartPolicy::Transient))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  let _ = children(&handle); // start closure (and registration) finished
  let exits = watch_exit(&jn.node, &pid);
  let addr = jn.node.whereis_addr::<Worker>("t").expect("registered");
  run(addr.call(WorkerCall::StopNormal, T)).unwrap();
  assert_eq!(await_exit(&exits), ExitReason::Normal);
  assert_eq!(pid_of(&handle, "t"), None);
  assert!(rx.try_recv().is_err());
}

#[test]
fn permanent_child_is_restarted_even_after_a_normal_exit() {
  let jn = JoinNode::new("permanent-normal@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(registered_worker_spec(worker_args("p", &tx)))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  let _ = children(&handle);
  let addr = jn.node.whereis_addr::<Worker>("p").expect("registered");
  run(addr.call(WorkerCall::StopNormal, T)).unwrap();
  let (_, again) = next_start(&rx);
  assert_ne!(pid, again);
}

#[test]
fn panicking_child_is_restarted_and_its_old_pid_rejects_sends() {
  let jn = JoinNode::new("panic-restart@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(registered_worker_spec(worker_args("p", &tx)))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  let _ = children(&handle);
  let exits = watch_exit(&jn.node, &pid);
  let old = jn.node.whereis_addr::<Worker>("p").expect("registered");
  let call = run(old.call(WorkerCall::Panic, T));
  assert!(
    matches!(call, Err(id_effect_process::CallError::Down(_))),
    "{call:?}"
  );
  assert_eq!(
    await_exit(&exits),
    ExitReason::Error("panic: worker panic".to_string())
  );
  let (_, again) = next_start(&rx);
  assert_ne!(pid, again);
  assert_eq!(pid_of(&handle, "p"), Some(again));
  assert_eq!(
    run(old.call(WorkerCall::Ping, T)),
    Err(id_effect_process::CallError::MailboxClosed)
  );
}

// ── Intensity and escalation ──────────────────────────────────────

#[test]
fn exceeding_restart_intensity_stops_the_supervisor_with_shutdown() {
  let jn = JoinNode::new("intensity@test");
  let (tx, rx) = mpsc::channel();
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(1, Duration::from_secs(60))
    .child(worker_spec(worker_args("a", &tx)))
    .start(&jn.node);
  let sup_exit = watch_exit(&jn.node, handle.pid());
  let (_, first) = next_start(&rx);
  crash(&jn.node, &first);
  let (_, second) = next_start(&rx); // one restart is within budget
  crash(&jn.node, &second);
  assert_eq!(await_exit(&sup_exit), ExitReason::Shutdown);
  assert!(rx.try_recv().is_err(), "no restart past the budget");
}

#[test]
fn child_supervisor_escalation_is_handled_by_the_parent() {
  let jn = JoinNode::new("escalate@test");
  let (tx, rx) = mpsc::channel();
  let leaf = worker_args("leaf", &tx);
  let root = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(5, Duration::from_secs(60))
    .child(ChildSpec::supervisor("inner", move || {
      SupervisorSpec::new(Strategy::OneForOne)
        .intensity(0, Duration::from_secs(60))
        .child(worker_spec(leaf.clone()))
    }))
    .start(&jn.node);
  let inner = pid_of(&root, "inner").expect("inner running");
  let inner_exit = watch_exit(&jn.node, &inner);
  let (_, leaf_pid) = next_start(&rx);
  crash(&jn.node, &leaf_pid);
  assert_eq!(await_exit(&inner_exit), ExitReason::Shutdown);
  let (id, fresh_leaf) = next_start(&rx);
  assert_eq!(id, "leaf", "root restarted the inner tree");
  assert_ne!(fresh_leaf, leaf_pid);
  assert_ne!(pid_of(&root, "inner"), Some(inner));
}

#[test]
fn supervisor_init_fails_when_a_child_cannot_start() {
  let jn = JoinNode::new("init-fail@test");
  let (tx, rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  run(probe.call((), T)).unwrap();
  let refuse: id_effect_process::StartFn =
    Arc::new(|_node, _sup| Err(StartError::Failed("no db".to_string())));
  let spec = SupervisorSpec::new(Strategy::OneForOne).child(ChildSpec::from_start("db", refuse));
  let sup = jn
    .node
    .spawn_linked_to::<SupervisorProcess>(probe.pid(), spec, HNil);
  match recv(&rx).expect("supervisor exit") {
    Info::Exit { from, reason } => {
      assert_eq!(&from, sup.pid());
      let expected = SupError::StartFailed("db".into(), StartError::Failed("no db".into()));
      assert_eq!(reason, ExitReason::error(expected));
    }
    _ => panic!("expected Info::Exit"),
  }
}

// ── Shutdown protocol ─────────────────────────────────────────────

#[test]
fn shutdown_stops_children_in_reverse_start_order() {
  let jn = JoinNode::new("order@test");
  let (started, started_rx) = mpsc::channel();
  let (stopped, stopped_rx) = mpsc::channel();
  let spec = |id| {
    worker_spec(WorkerArgs {
      id,
      started: started.clone(),
      stopped: Some(stopped.clone()),
      trap: false,
    })
  };
  let root = SupervisorSpec::new(Strategy::OneForOne)
    .child(spec("a"))
    .child(spec("b"))
    .child(spec("c"))
    .start(&jn.node);
  for _ in 0..3 {
    next_start(&started_rx);
  }
  run::<(), Never>(root.stop(&jn.node, ShutdownPolicy::Infinity)).expect("stopped");
  let order: Vec<_> = stopped_rx.try_iter().collect();
  assert_eq!(order, ["c", "b", "a"]);
}

#[test]
fn shutdown_timeout_escalates_to_kill_for_a_child_that_traps_exits() {
  let jn = JoinNode::new("stubborn@test");
  let (tx, rx) = mpsc::channel();
  let stubborn = WorkerArgs {
    trap: true,
    ..worker_args("stubborn", &tx)
  };
  let root = SupervisorSpec::new(Strategy::OneForOne)
    .child(worker_spec(stubborn).with_shutdown(ShutdownPolicy::Timeout(Duration::from_millis(50))))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  let exits = watch_exit(&jn.node, &pid);
  let stop = root
    .stop::<Never, HNil>(&jn.node, ShutdownPolicy::Infinity)
    .fork_root(Arc::clone(jn.node.runtime().executor()));
  settle();
  assert!(
    exits.try_recv().is_err(),
    "the trapped shutdown waits for the deadline"
  );
  advance(Duration::from_millis(49));
  assert!(exits.try_recv().is_err(), "virtual 49 ms: still alive");
  advance(Duration::from_millis(1));
  assert_eq!(await_exit(&exits), ExitReason::Killed);
  assert!(stop.is_done());
}

#[test]
fn brutal_kill_child_policy_kills_immediately() {
  let jn = JoinNode::new("brutal@test");
  let (tx, rx) = mpsc::channel();
  let stubborn = WorkerArgs {
    trap: true,
    ..worker_args("b", &tx)
  };
  let root = SupervisorSpec::new(Strategy::OneForOne)
    .child(worker_spec(stubborn).with_shutdown(ShutdownPolicy::BrutalKill))
    .start(&jn.node);
  let (_, pid) = next_start(&rx);
  let exits = watch_exit(&jn.node, &pid);
  assert_eq!(run(root.terminate_child("b", T)), Ok(true));
  assert_eq!(await_exit(&exits), ExitReason::Killed);
  assert_eq!(run(root.count_children(T)), Ok(0));
}

// ── Dynamic membership on a static supervisor ─────────────────────

#[test]
fn start_child_with_a_failing_start_reports_the_error() {
  let jn = JoinNode::new("start-fail@test");
  let root = SupervisorSpec::new(Strategy::OneForOne).start(&jn.node);
  let refuse: id_effect_process::StartFn =
    Arc::new(|_node, _sup| Err(StartError::Failed("busy".to_string())));
  let out = run(root.start_child(ChildSpec::from_start("x", refuse), T));
  assert_eq!(out, Ok(Err(StartError::Failed("busy".to_string()))));
  assert_eq!(run(root.count_children(T)), Ok(0));
}

#[test]
fn terminate_child_with_an_unknown_id_is_false() {
  let jn = JoinNode::new("term-unknown@test");
  let root = SupervisorSpec::new(Strategy::OneForOne).start(&jn.node);
  assert_eq!(run(root.terminate_child("ghost", T)), Ok(false));
}

// ── DynamicSupervisor ─────────────────────────────────────────────

fn dyn_start(started: &mpsc::Sender<(&'static str, Pid)>) -> StartChildFn {
  let args = worker_args("dyn", started);
  Arc::new(move |node, sup| {
    Ok(
      node
        .spawn_linked_to::<Worker>(sup, args.clone(), ())
        .pid()
        .clone(),
    )
  })
}

#[test]
fn dynamic_terminate_child_stops_and_forgets_it() {
  let jn = JoinNode::new("dyn-term@test");
  let (tx, rx) = mpsc::channel();
  let sup = DynamicSupervisor::start(&jn.node, DynamicSupervisorSpec::default());
  let pid = run(sup.start_dyn_child(dyn_start(&tx), T))
    .unwrap()
    .unwrap();
  next_start(&rx);
  let exits = watch_exit(&jn.node, &pid);
  assert_eq!(run(sup.terminate_dyn_child(pid.clone(), T)), Ok(true));
  assert_eq!(await_exit(&exits), ExitReason::Shutdown);
  assert_eq!(run(sup.count_dyn_children(T)), Ok(0));
  assert_eq!(run(sup.terminate_dyn_child(pid, T)), Ok(false));
  assert!(rx.try_recv().is_err(), "an expected exit is not restarted");
}

#[test]
fn dynamic_temporary_children_are_dropped_after_a_crash() {
  let jn = JoinNode::new("dyn-temp@test");
  let (tx, rx) = mpsc::channel();
  let sup = DynamicSupervisor::start(
    &jn.node,
    DynamicSupervisorSpec {
      restart: RestartPolicy::Temporary,
      ..DynamicSupervisorSpec::default()
    },
  );
  let pid = run(sup.start_dyn_child(dyn_start(&tx), T))
    .unwrap()
    .unwrap();
  next_start(&rx);
  crash(&jn.node, &pid);
  assert_eq!(run(sup.count_dyn_children(T)), Ok(0));
  assert!(rx.try_recv().is_err());
}

#[test]
fn dynamic_supervisor_escalates_when_intensity_is_exhausted() {
  let jn = JoinNode::new("dyn-intensity@test");
  let (tx, rx) = mpsc::channel();
  let sup = DynamicSupervisor::start(
    &jn.node,
    DynamicSupervisorSpec {
      restart: RestartPolicy::Permanent,
      max_restarts: 0,
      ..DynamicSupervisorSpec::default()
    },
  );
  let sup_exit = watch_exit(&jn.node, sup.pid());
  let pid = run(sup.start_dyn_child(dyn_start(&tx), T))
    .unwrap()
    .unwrap();
  next_start(&rx);
  crash(&jn.node, &pid);
  assert_eq!(await_exit(&sup_exit), ExitReason::Shutdown);
}

#[test]
fn stopping_a_dynamic_supervisor_stops_its_children() {
  let jn = JoinNode::new("dyn-stop@test");
  let (tx, rx) = mpsc::channel();
  let sup = DynamicSupervisor::start(&jn.node, DynamicSupervisorSpec::default());
  let mut exits = Vec::new();
  for _ in 0..2 {
    let pid = run(sup.start_dyn_child(dyn_start(&tx), T))
      .unwrap()
      .unwrap();
    next_start(&rx);
    exits.push(watch_exit(&jn.node, &pid));
  }
  assert_eq!(run(sup.count_dyn_children(T)), Ok(2));
  jn.node.exit(sup.pid(), ExitReason::Shutdown);
  for rx in &exits {
    assert_eq!(await_exit(rx), ExitReason::Shutdown);
  }
}
