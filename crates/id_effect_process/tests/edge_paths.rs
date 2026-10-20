//! Edge paths of the node, the supervisors and the mailbox: failing restarts that exhaust the
//! intensity, `start_link` from inside a process, signals to unknown pids, idle waits that end
//! in a message or a kill, and a selective receive cut short by a kill.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use common::{JoinNode, Probe, Recorder, T, advance, await_exit, recv, run, settle, watch_exit};
use id_effect::{Effect, succeed};
use id_effect_process::{
  CallError, ChildSpec, DynamicSupervisor, DynamicSupervisorSpec, ExitReason, Info, Next, NodeName,
  Pid, ProcMsg, Process, ProcessCtx, ProcessNode, RestartPolicy, SpawnOptions, StartChildFn,
  StartError, Strategy, SupervisorSpec,
};

/// A start closure that spawns a linked [`Recorder`] the first time and fails every time after.
fn first_start_only() -> StartChildFn {
  let starts = Arc::new(AtomicUsize::new(0));
  Arc::new(move |node: &ProcessNode, sup: &Pid| {
    if starts.fetch_add(1, Ordering::SeqCst) == 0 {
      Ok(
        node
          .spawn_linked_to::<Recorder>(sup, None, ())
          .pid()
          .clone(),
      )
    } else {
      Err(StartError::Failed("start refused".into()))
    }
  })
}

fn recorder_spec(id: &str) -> ChildSpec {
  ChildSpec::worker::<Recorder, _>(id.to_string(), || (None, ()))
}

/// Crash `pid` abnormally and wait for its finalization.
fn crash(node: &ProcessNode, pid: &Pid) {
  let exits = watch_exit(node, pid);
  node.exit(pid, ExitReason::Error("induced".into()));
  await_exit(&exits);
}

// ── Node ───────────────────────────────────────────────────────────

#[test]
fn node_on_the_global_runtime_starts_empty() {
  let node = ProcessNode::new(NodeName::new("global@test"));
  assert_eq!(node.name(), &NodeName::new("global@test"));
  assert!(node.whereis("nobody").is_none());
}

#[test]
fn signals_to_unknown_pids_are_no_ops() {
  let jn = JoinNode::new("ghost@test");
  let ghost_a = Pid::from_parts(jn.node.name().clone(), 900, 0);
  let ghost_b = Pid::from_parts(jn.node.name().clone(), 901, 0);
  jn.node.exit(&ghost_a, ExitReason::Killed);
  jn.node.link(&ghost_a, &ghost_b);
  assert_eq!(
    run(jn.node.await_exit::<(), _>(&ghost_a)),
    Ok(ExitReason::NoProc)
  );
}

#[test]
fn await_exit_on_a_live_process_yields_its_reason() {
  let jn = JoinNode::new("await@test");
  let rec = jn.node.spawn::<Recorder>(None, ());
  settle();
  jn.node.exit(rec.pid(), ExitReason::Killed);
  assert_eq!(
    run(jn.node.await_exit::<(), _>(rec.pid())),
    Ok(ExitReason::Killed)
  );
}

#[test]
fn linking_an_unknown_pid_to_a_live_one_sends_noproc() {
  let jn = JoinNode::new("noproc@test");
  let (tx, rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  settle();
  let ghost = Pid::from_parts(jn.node.name().clone(), 900, 0);
  jn.node.link(&ghost, probe.pid());
  match recv(&rx).expect("exit info") {
    Info::Exit { from, reason } => {
      assert_eq!(from, ghost);
      assert_eq!(reason, ExitReason::NoProc);
    }
    other => panic!("unexpected info: {other:?}"),
  }
}

#[test]
fn spawn_linked_to_with_options_links_and_bounds_the_child() {
  let jn = JoinNode::new("opts@test");
  let (tx, rx) = mpsc::channel();
  let parent = jn.node.spawn::<Probe>(tx, ());
  settle();
  let child = jn.node.spawn_linked_to_with::<Recorder>(
    parent.pid(),
    None,
    (),
    SpawnOptions::new().mailbox_capacity(1),
  );
  settle();
  crash(&jn.node, child.pid());
  match recv(&rx).expect("exit info") {
    Info::Exit { from, reason } => {
      assert_eq!(&from, child.pid());
      assert_eq!(reason, ExitReason::Error("induced".into()));
    }
    other => panic!("unexpected info: {other:?}"),
  }
}

// ── Static supervisor ──────────────────────────────────────────────

#[test]
fn failing_restart_exhausts_intensity_one_for_one() {
  let jn = JoinNode::new("sup-ofo@test");
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .intensity(1, Duration::from_secs(60))
    .child(ChildSpec::from_start("flaky", first_start_only()))
    .start(&jn.node);
  let children = run(handle.which_children(T)).expect("children");
  let child = children[0].1.clone().expect("child started");
  let sup_exit = watch_exit(&jn.node, handle.pid());
  crash(&jn.node, &child);
  assert_eq!(await_exit(&sup_exit), ExitReason::Shutdown);
}

#[test]
fn failing_restart_exhausts_intensity_one_for_all() {
  let jn = JoinNode::new("sup-ofa@test");
  let handle = SupervisorSpec::new(Strategy::OneForAll)
    .intensity(1, Duration::from_secs(60))
    .child(ChildSpec::from_start("flaky", first_start_only()))
    .child(recorder_spec("steady"))
    .start(&jn.node);
  let children = run(handle.which_children(T)).expect("children");
  let flaky = children[0].1.clone().expect("flaky started");
  let sup_exit = watch_exit(&jn.node, handle.pid());
  crash(&jn.node, &flaky);
  assert_eq!(await_exit(&sup_exit), ExitReason::Shutdown);
}

#[test]
fn one_for_all_restart_skips_temporary_children() {
  let jn = JoinNode::new("sup-temp@test");
  let handle = SupervisorSpec::new(Strategy::OneForAll)
    .intensity(5, Duration::from_secs(60))
    .child(recorder_spec("permanent"))
    .child(recorder_spec("temporary").with_restart(RestartPolicy::Temporary))
    .start(&jn.node);
  let before = run(handle.which_children(T)).expect("children");
  let permanent = before[0].1.clone().expect("permanent started");
  crash(&jn.node, &permanent);
  let after = run(handle.which_children(T)).expect("children");
  assert!(after[0].1.is_some());
  assert_ne!(after[0].1, before[0].1, "permanent child restarted");
  assert_eq!(after[1].1, None, "temporary child stays stopped");
}

#[test]
fn terminating_a_child_whose_process_is_gone_succeeds() {
  let jn = JoinNode::new("sup-gone@test");
  let ghost = Pid::from_parts(jn.node.name().clone(), 900, 0);
  let start = Arc::new(move |_: &ProcessNode, _: &Pid| Ok(ghost.clone()));
  let handle = SupervisorSpec::new(Strategy::OneForOne)
    .child(ChildSpec::from_start("ghost", start))
    .start(&jn.node);
  assert_eq!(run(handle.terminate_child("ghost", T)), Ok(true));
  assert_eq!(run(handle.count_children(T)), Ok(0));
}

#[test]
fn supervisor_ignores_user_infos() {
  let jn = JoinNode::new("sup-info@test");
  let handle = SupervisorSpec::new(Strategy::OneForOne).start(&jn.node);
  assert_eq!(run(handle.addr().send(())), Ok(()));
  assert_eq!(run(handle.count_children(T)), Ok(0));
}

// ── start_link from inside a process ───────────────────────────────

/// Starts supervisors linked to itself, then stops normally on request.
struct Parent;

enum ParentCall {
  StartSupervisor,
  StartDynamic,
  Stop,
}

impl Process for Parent {
  type Args = ();
  type Env = ();
  type Call = ParentCall;
  type Reply = Option<Pid>;
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(_args: (), _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    succeed(Parent)
  }

  fn handle_call(
    self,
    msg: ParentCall,
    ctx: ProcessCtx,
  ) -> Effect<(Option<Pid>, Next<Self>), String, ()> {
    match msg {
      ParentCall::StartSupervisor => {
        let sup = SupervisorSpec::new(Strategy::OneForOne).start_link(&ctx);
        succeed((Some(sup.pid().clone()), Next::Continue(self)))
      }
      ParentCall::StartDynamic => {
        let sup = DynamicSupervisor::start_link(&ctx, DynamicSupervisorSpec::default());
        succeed((Some(sup.pid().clone()), Next::Continue(self)))
      }
      ParentCall::Stop => succeed((None, Next::Stop(ExitReason::Normal, self))),
    }
  }
}

#[test]
fn linked_supervisors_survive_a_normal_parent_exit() {
  let jn = JoinNode::new("parent@test");
  let parent = jn.node.spawn::<Parent>((), ());
  let sup = run(parent.call(ParentCall::StartSupervisor, T))
    .expect("call")
    .expect("supervisor pid");
  let dynamic = run(parent.call(ParentCall::StartDynamic, T))
    .expect("call")
    .expect("dynamic pid");
  let parent_exit = watch_exit(&jn.node, parent.pid());
  assert_eq!(run(parent.call(ParentCall::Stop, T)), Ok(None));
  assert_eq!(await_exit(&parent_exit), ExitReason::Normal);
  settle();
  // A `Normal` exit from a linked non-child is ignored by both supervisor kinds.
  assert!(jn.node.table().lookup(&sup).is_some());
  assert!(jn.node.table().lookup(&dynamic).is_some());
}

// ── Dynamic supervisor ─────────────────────────────────────────────

fn recorder_start() -> StartChildFn {
  Arc::new(|node: &ProcessNode, sup: &Pid| {
    Ok(
      node
        .spawn_linked_to::<Recorder>(sup, None, ())
        .pid()
        .clone(),
    )
  })
}

#[test]
fn dynamic_start_failure_and_unknown_terminate() {
  let jn = JoinNode::new("dyn-fail@test");
  let sup = DynamicSupervisor::start(&jn.node, DynamicSupervisorSpec::default());
  let refuse: StartChildFn =
    Arc::new(|_: &ProcessNode, _: &Pid| Err(StartError::Failed("no".into())));
  assert_eq!(
    run(sup.start_dyn_child(refuse, T)),
    Ok(Err(StartError::Failed("no".into())))
  );
  let ghost = Pid::from_parts(jn.node.name().clone(), 900, 0);
  assert_eq!(run(sup.terminate_dyn_child(ghost, T)), Ok(false));
  assert_eq!(run(sup.send(())), Ok(()));
  assert_eq!(run(sup.count_dyn_children(T)), Ok(0));
}

#[test]
fn dynamic_failing_restart_exhausts_intensity() {
  let jn = JoinNode::new("dyn-exhaust@test");
  let spec = DynamicSupervisorSpec {
    restart: RestartPolicy::Permanent,
    max_restarts: 1,
    ..DynamicSupervisorSpec::default()
  };
  let sup = DynamicSupervisor::start(&jn.node, spec);
  let child = run(sup.start_dyn_child(first_start_only(), T))
    .expect("call")
    .expect("child pid");
  let sup_exit = watch_exit(&jn.node, sup.pid());
  crash(&jn.node, &child);
  assert_eq!(await_exit(&sup_exit), ExitReason::Shutdown);
}

#[test]
fn dynamic_restart_window_forgets_old_restarts() {
  let jn = JoinNode::new("dyn-window@test");
  let spec = DynamicSupervisorSpec {
    restart: RestartPolicy::Permanent,
    max_restarts: 1,
    within: Duration::from_secs(1),
    ..DynamicSupervisorSpec::default()
  };
  let sup = DynamicSupervisor::start(&jn.node, spec);
  run(sup.start_dyn_child(recorder_start(), T))
    .expect("call")
    .expect("child pid");
  for _ in 0..2 {
    let child = jn
      .node
      .table()
      .pids()
      .into_iter()
      .find(|p| p != sup.pid())
      .expect("a live child");
    crash(&jn.node, &child);
    advance(Duration::from_secs(5));
  }
  assert!(sup.is_alive(), "restarts outside the window do not count");
  assert_eq!(run(sup.count_dyn_children(T)), Ok(1));
}

// ── Mailbox waits ──────────────────────────────────────────────────

/// Arms an idle timeout in `init`; `Wait` blocks in a selective receive for a message that
/// never arrives.
struct Idler {
  timeouts: usize,
}

enum IdlerCall {
  Timeouts,
  Wait,
}

impl Process for Idler {
  type Args = ();
  type Env = ();
  type Call = IdlerCall;
  type Reply = usize;
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(_args: (), ctx: ProcessCtx) -> Effect<Self, String, ()> {
    ctx.set_idle_timeout(Duration::from_secs(30));
    succeed(Idler { timeouts: 0 })
  }

  fn handle_call(self, msg: IdlerCall, ctx: ProcessCtx) -> Effect<(usize, Next<Self>), String, ()> {
    match msg {
      IdlerCall::Timeouts => succeed((self.timeouts, Next::Continue(self))),
      IdlerCall::Wait => ctx
        .receive::<Idler, ()>(
          |m| matches!(m, ProcMsg::Info(Info::User(()))),
          Duration::from_secs(3600),
        )
        .map_err(|e| e.to_string())
        .map(move |_| (0, Next::Continue(self))),
    }
  }

  fn handle_info(mut self, msg: Info<()>, _ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    if matches!(msg, Info::Timeout) {
      self.timeouts += 1;
    }
    succeed(Next::Continue(self))
  }
}

#[test]
fn idle_wait_takes_a_message_that_arrives_before_the_deadline() {
  let jn = JoinNode::new("idle-msg@test");
  let idler = jn.node.spawn::<Idler>((), ());
  settle();
  assert_eq!(run(idler.call(IdlerCall::Timeouts, T)), Ok(0));
  advance(Duration::from_secs(31));
  assert_eq!(run(idler.call(IdlerCall::Timeouts, T)), Ok(1));
}

#[test]
fn kill_during_an_idle_wait_closes_the_mailbox() {
  let jn = JoinNode::new("idle-kill@test");
  let idler = jn.node.spawn::<Idler>((), ());
  settle();
  let exit = watch_exit(&jn.node, idler.pid());
  jn.node
    .exit(idler.pid(), ExitReason::Error("stop idling".into()));
  assert_eq!(await_exit(&exit), ExitReason::Error("stop idling".into()));
}

#[test]
fn kill_during_a_selective_receive_fails_the_waiting_call() {
  let jn = JoinNode::new("recv-kill@test");
  let idler = jn.node.spawn::<Idler>((), ());
  settle();
  let exit = watch_exit(&jn.node, idler.pid());
  let pending = jn
    .node
    .runtime()
    .fork(idler.call::<id_effect::HNil>(IdlerCall::Wait, T));
  settle();
  jn.node
    .exit(idler.pid(), ExitReason::Error("stop waiting".into()));
  let reason = await_exit(&exit);
  assert!(reason.is_abnormal(), "{reason:?}");
  drop(pending);
  assert_eq!(
    run(idler.call(IdlerCall::Timeouts, T)),
    Err(CallError::MailboxClosed)
  );
}
