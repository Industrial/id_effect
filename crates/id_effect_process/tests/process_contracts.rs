//! Deterministic contract tests for mailboxes, exit signals, links,
//! monitors, `terminate`, and the name registry (ADR 0009 tables).
//!
//! No sleeps: see `common` for the synchronization fixtures.

mod common;

use std::sync::mpsc;

use common::{JoinNode, Probe, RecCall, Recorder, T, await_exit, watch_exit};
use id_effect::{Cause, Effect, run_blocking};
use id_effect_process::{
  Addr, CallError, ExitReason, Info, MonitorRef, Next, NodeName, Pid, Process, ProcessCtx,
  SendError,
};

fn log(addr: &Addr<Recorder>) -> Vec<i64> {
  run_blocking(addr.call(RecCall::Log, T), ()).expect("log call")
}

fn crash(addr: &Addr<Recorder>) {
  let _ = run_blocking(addr.call(RecCall::Crash, T), ());
}

// ── Mailbox ordering and dead-letter behaviour ─────────────────────

#[test]
fn mailbox_preserves_fifo_order_across_many_casts() {
  let jn = JoinNode::new("fifo@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  for n in 0..200 {
    run_blocking(addr.cast(n), ()).unwrap();
  }
  assert_eq!(log(&addr), (0..200).collect::<Vec<_>>());
}

#[test]
fn send_now_interleaves_with_casts_in_sender_order() {
  let jn = JoinNode::new("interleave@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  run_blocking(addr.cast(1), ()).unwrap();
  assert!(addr.send_now(2));
  run_blocking(addr.send(3), ()).unwrap();
  assert!(addr.send_now(4));
  assert_eq!(log(&addr), vec![1, 2, 3, 4]);
}

#[test]
fn messages_to_a_dead_process_are_rejected_not_lost_silently() {
  let jn = JoinNode::new("deadletter@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let _ = run_blocking(addr.call(RecCall::Stop, T), ());
  jn.join(0);
  assert!(!addr.is_alive());
  assert!(!addr.send_now(1));
  assert_eq!(run_blocking(addr.cast(1), ()), Err(SendError::Rejected));
  assert_eq!(run_blocking(addr.send(1), ()), Err(SendError::Rejected));
  let call = run_blocking(addr.call(RecCall::Log, T), ());
  assert!(
    matches!(
      call,
      Err(CallError::MailboxClosed) | Err(CallError::Down(ExitReason::NoProc))
    ),
    "got {call:?}"
  );
}

#[test]
fn cloned_addresses_reach_the_same_process() {
  let jn = JoinNode::new("clone@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let other = addr.clone();
  assert_eq!(addr.pid(), other.pid());
  run_blocking(other.cast(7), ()).unwrap();
  assert_eq!(log(&addr), vec![7]);
}

// ── Monitors ──────────────────────────────────────────────────────

#[test]
fn monitor_reports_the_crash_reason() {
  let jn = JoinNode::new("mon-crash@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let exits = watch_exit(&jn.node, addr.pid());
  crash(&addr);
  match await_exit(&exits) {
    ExitReason::Error(msg) => assert!(msg.contains("recorder crash"), "{msg}"),
    other => panic!("expected Error, got {other:?}"),
  }
}

#[test]
fn monitor_reports_normal_for_a_graceful_stop() {
  let jn = JoinNode::new("mon-normal@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let exits = watch_exit(&jn.node, addr.pid());
  let _ = run_blocking(addr.call(RecCall::Stop, T), ());
  assert_eq!(await_exit(&exits), ExitReason::Normal);
}

#[test]
fn monitor_reports_killed_for_an_untrappable_kill() {
  let jn = JoinNode::new("mon-kill@test");
  let (tx, _rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  run_blocking(probe.call((), T), ()).unwrap(); // init done: trapping
  let exits = watch_exit(&jn.node, probe.pid());
  jn.node.exit(probe.pid(), ExitReason::Killed);
  assert_eq!(await_exit(&exits), ExitReason::Killed);
}

#[test]
fn monitoring_a_dead_pid_fires_noproc_synchronously() {
  let jn = JoinNode::new("mon-noproc@test");
  let ghost = Pid::from_parts(NodeName::new("mon-noproc@test"), 9_999, 0);
  let exits = watch_exit(&jn.node, &ghost);
  assert_eq!(exits.try_recv(), Ok(ExitReason::NoProc));
}

#[test]
fn every_monitor_fires_exactly_once() {
  let jn = JoinNode::new("mon-once@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let first = watch_exit(&jn.node, addr.pid());
  let second = watch_exit(&jn.node, addr.pid());
  crash(&addr);
  jn.join(0);
  assert!(matches!(first.try_recv(), Ok(ExitReason::Error(_))));
  assert!(matches!(second.try_recv(), Ok(ExitReason::Error(_))));
  assert!(first.try_recv().is_err(), "no duplicate delivery");
  assert!(second.try_recv().is_err(), "no duplicate delivery");
}

#[test]
fn demonitor_before_exit_suppresses_delivery() {
  let jn = JoinNode::new("demon@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let (tx, rx) = mpsc::channel();
  let monitor = jn.node.monitor_with_sink(
    addr.pid(),
    Box::new(move |_m, _p, reason| {
      let _ = tx.send(reason);
    }),
  );
  jn.node.demonitor(addr.pid(), monitor);
  crash(&addr);
  jn.join(0);
  assert!(rx.try_recv().is_err(), "demonitored sink must not fire");
}

#[test]
fn monitor_refs_are_unique_and_round_trip() {
  let a = MonitorRef::fresh();
  let b = MonitorRef::fresh();
  assert_ne!(a, b);
  assert_eq!(MonitorRef::from_inner(a.into_inner()), a);
  assert_eq!(a.to_string(), format!("#Ref<{}>", a.into_inner()));
}

// ── Links and exit signals ─────────────────────────────────────────

#[test]
fn abnormal_exit_cascades_along_a_link_chain_with_the_same_reason() {
  let jn = JoinNode::new("chain@test");
  let a = jn.node.spawn::<Recorder>(None, ());
  let b = jn.node.spawn::<Recorder>(None, ());
  let c = jn.node.spawn::<Recorder>(None, ());
  jn.node.link(a.pid(), b.pid());
  jn.node.link(b.pid(), c.pid());
  let c_exit = watch_exit(&jn.node, c.pid());
  crash(&a);
  match await_exit(&c_exit) {
    ExitReason::Error(msg) => assert!(msg.contains("recorder crash"), "{msg}"),
    other => panic!("expected propagated Error, got {other:?}"),
  }
}

#[test]
fn unlink_stops_the_cascade() {
  let jn = JoinNode::new("unlink@test");
  let a = jn.node.spawn::<Recorder>(None, ());
  let b = jn.node.spawn::<Recorder>(None, ());
  jn.node.link(a.pid(), b.pid());
  jn.node.unlink(b.pid(), a.pid());
  crash(&a);
  jn.join(0);
  assert!(b.is_alive());
  run_blocking(b.cast(5), ()).unwrap();
  assert_eq!(log(&b), vec![5]);
}

#[test]
fn exit_normal_is_ignored_by_a_non_trapping_process() {
  let jn = JoinNode::new("exit-normal@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  jn.node.exit(addr.pid(), ExitReason::Normal);
  run_blocking(addr.cast(1), ()).unwrap();
  assert_eq!(log(&addr), vec![1]);
  assert!(addr.is_alive());
}

#[test]
fn trappable_exit_reaches_a_trapping_process_as_a_message() {
  let jn = JoinNode::new("exit-trap@test");
  let (tx, rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  run_blocking(probe.call((), T), ()).unwrap();
  jn.node.exit(probe.pid(), ExitReason::Shutdown);
  match rx.recv_timeout(T).expect("exit message") {
    Info::Exit { from, reason } => {
      assert_eq!(&from, probe.pid());
      assert_eq!(reason, ExitReason::Shutdown);
    }
    _ => panic!("expected Info::Exit"),
  }
  run_blocking(probe.call((), T), ()).expect("probe survives");
}

#[test]
fn trapping_process_learns_the_linked_peer_crash_reason() {
  let jn = JoinNode::new("trap-link@test");
  let (tx, rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  run_blocking(probe.call((), T), ()).unwrap();
  let peer = jn.node.spawn_linked_to::<Recorder>(probe.pid(), None, ());
  crash(&peer);
  match rx.recv_timeout(T).expect("exit message") {
    Info::Exit { from, reason } => {
      assert_eq!(&from, peer.pid());
      assert!(matches!(reason, ExitReason::Error(_)), "{reason:?}");
    }
    _ => panic!("expected Info::Exit"),
  }
}

#[test]
fn spawn_linked_to_a_dead_parent_dies_with_noproc() {
  let jn = JoinNode::new("orphan@test");
  let ghost = Pid::from_parts(NodeName::new("orphan@test"), 4_242, 0);
  let child = jn.node.spawn_linked_to::<Recorder>(&ghost, None, ());
  let exits = watch_exit(&jn.node, child.pid());
  assert_eq!(await_exit(&exits), ExitReason::NoProc);
}

// ── init / terminate ──────────────────────────────────────────────

struct FailingInit;

impl Process for FailingInit {
  type Args = ();
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(_: (), _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    id_effect::fail("no config".to_string())
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), String, ()> {
    id_effect::succeed(((), Next::Continue(self)))
  }
}

#[test]
fn init_failure_exits_with_the_init_error() {
  let jn = JoinNode::new("init-fail@test");
  let (tx, rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  run_blocking(probe.call((), T), ()).unwrap();
  let child = jn.node.spawn_linked_to::<FailingInit>(probe.pid(), (), ());
  match rx.recv_timeout(T).expect("exit message") {
    Info::Exit { from, reason } => {
      assert_eq!(&from, child.pid());
      assert_eq!(reason, ExitReason::error("no config"));
    }
    _ => panic!("expected Info::Exit"),
  }
}

#[test]
fn terminate_receives_the_cooperative_kill_reason() {
  let jn = JoinNode::new("term-reason@test");
  let (tx, rx) = mpsc::channel();
  let addr = jn.node.spawn::<Recorder>(Some(tx), ());
  let _ = log(&addr);
  jn.node.exit(addr.pid(), ExitReason::Shutdown);
  assert_eq!(rx.recv_timeout(T), Ok(ExitReason::Shutdown));
}

#[test]
fn terminate_runs_with_normal_after_a_graceful_stop() {
  let jn = JoinNode::new("term-normal@test");
  let (tx, rx) = mpsc::channel();
  let addr = jn.node.spawn::<Recorder>(Some(tx), ());
  let _ = run_blocking(addr.call(RecCall::Stop, T), ());
  assert_eq!(rx.recv_timeout(T), Ok(ExitReason::Normal));
}

#[test]
fn terminate_is_skipped_when_a_handler_crashes() {
  let jn = JoinNode::new("term-crash@test");
  let (tx, rx) = mpsc::channel();
  let addr = jn.node.spawn::<Recorder>(Some(tx), ());
  crash(&addr);
  jn.join(0);
  assert!(rx.try_recv().is_err(), "state was consumed by the crash");
}

// ── Panics are crashes ────────────────────────────────────────────

#[test]
fn handler_panic_fails_the_call_fires_monitors_and_closes_the_mailbox() {
  let jn = JoinNode::new("panic@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let exits = watch_exit(&jn.node, addr.pid());
  let call = run_blocking(addr.call(RecCall::Panic, T), ());
  assert_eq!(
    call,
    Err(CallError::Down(ExitReason::Error(
      "panic: recorder panic".to_string()
    )))
  );
  assert_eq!(
    await_exit(&exits),
    ExitReason::Error("panic: recorder panic".to_string())
  );
  jn.join(0); // the process thread itself did not unwind
  assert!(!addr.is_alive());
  assert_eq!(run_blocking(addr.send(1), ()), Err(SendError::Rejected));
  assert!(!addr.send_now(1));
}

#[test]
fn handler_panic_cascades_over_links() {
  let jn = JoinNode::new("panic-link@test");
  let a = jn.node.spawn::<Recorder>(None, ());
  let b = jn.node.spawn::<Recorder>(None, ());
  jn.node.link(a.pid(), b.pid());
  let b_exit = watch_exit(&jn.node, b.pid());
  let _ = run_blocking(a.call(RecCall::Panic, T), ());
  assert_eq!(
    await_exit(&b_exit),
    ExitReason::Error("panic: recorder panic".to_string())
  );
}

struct PanickingInit;

impl Process for PanickingInit {
  type Args = ();
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = String;

  fn init(_: (), _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    panic!("init exploded")
  }

  fn handle_call(self, _: (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), String, ()> {
    id_effect::succeed(((), Next::Continue(self)))
  }
}

#[test]
fn init_panic_exits_with_the_panic_message() {
  let jn = JoinNode::new("init-panic@test");
  let (tx, rx) = mpsc::channel();
  let probe = jn.node.spawn::<Probe>(tx, ());
  run_blocking(probe.call((), T), ()).unwrap();
  let child = jn
    .node
    .spawn_linked_to::<PanickingInit>(probe.pid(), (), ());
  match rx.recv_timeout(T).expect("exit message") {
    Info::Exit { from, reason } => {
      assert_eq!(&from, child.pid());
      assert_eq!(
        reason,
        ExitReason::Error("panic: init exploded".to_string())
      );
    }
    _ => panic!("expected Info::Exit"),
  }
}

// ── Registry ──────────────────────────────────────────────────────

#[test]
fn registering_a_dead_process_fails() {
  let jn = JoinNode::new("reg-dead@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  let _ = run_blocking(addr.call(RecCall::Stop, T), ());
  jn.join(0);
  assert!(!jn.node.register("late", &addr));
  assert_eq!(jn.node.whereis("late"), None);
}

#[test]
fn unregister_frees_the_name_and_keeps_the_process() {
  let jn = JoinNode::new("reg-free@test");
  let a = jn.node.spawn::<Recorder>(None, ());
  let b = jn.node.spawn::<Recorder>(None, ());
  assert!(jn.node.register("svc", &a));
  jn.node.unregister("svc");
  assert_eq!(jn.node.whereis("svc"), None);
  assert!(jn.node.register("svc", &b), "name is reusable");
  assert!(a.is_alive());
  assert_eq!(jn.node.whereis("svc"), Some(b.pid().clone()));
}

#[test]
fn typed_lookup_with_the_wrong_behaviour_is_none() {
  let jn = JoinNode::new("reg-type@test");
  let addr = jn.node.spawn::<Recorder>(None, ());
  assert!(jn.node.register("rec", &addr));
  assert!(jn.node.whereis_addr::<Probe>("rec").is_none());
  assert!(jn.node.whereis_addr::<Recorder>("rec").is_some());
}

#[test]
fn process_table_tracks_live_processes() {
  let jn = JoinNode::new("table@test");
  assert!(jn.node.table().is_empty());
  let a = jn.node.spawn::<Recorder>(None, ());
  let _b = jn.node.spawn::<Recorder>(None, ());
  assert_eq!(jn.node.table().len(), 2);
  assert!(jn.node.table().pids().contains(a.pid()));
  let _ = run_blocking(a.call(RecCall::Stop, T), ());
  jn.join(0);
  assert_eq!(jn.node.table().len(), 1);
  assert!(!jn.node.table().pids().contains(a.pid()));
}

// ── Plain data: pids, reasons, errors ──────────────────────────────

#[test]
fn pid_parts_and_display_round_trip() {
  let pid = Pid::from_parts(NodeName::new("a@host"), 12, 3);
  assert_eq!(pid.node().as_str(), "a@host");
  assert_eq!((pid.local_id(), pid.incarnation()), (12, 3));
  assert_eq!(pid.to_string(), "<a@host.12.3>");
  assert_eq!(NodeName::local().to_string(), "nonode@nohost");
}

#[test]
fn spawned_pids_are_never_reused() {
  let jn = JoinNode::new("pids@test");
  let a = jn.node.spawn::<Recorder>(None, ());
  let _ = run_blocking(a.call(RecCall::Stop, T), ());
  jn.join(0);
  let b = jn.node.spawn::<Recorder>(None, ());
  assert_ne!(a.pid(), b.pid());
  assert!(b.pid().local_id() > a.pid().local_id());
  assert_eq!(b.pid().node(), jn.node.name());
}

#[test]
fn exit_reason_classification_matches_beam() {
  assert!(!ExitReason::Normal.is_abnormal());
  assert!(ExitReason::Normal.is_graceful());
  assert!(ExitReason::Shutdown.is_abnormal());
  assert!(ExitReason::Shutdown.is_graceful());
  for reason in [
    ExitReason::Killed,
    ExitReason::NoProc,
    ExitReason::Interrupted,
    ExitReason::NoConnection,
    ExitReason::error("x"),
  ] {
    assert!(reason.is_abnormal(), "{reason}");
    assert!(!reason.is_graceful(), "{reason}");
  }
  assert_eq!(ExitReason::error("bad").to_string(), "error: \"bad\"");
  assert_eq!(ExitReason::Killed.to_string(), "killed");
}

#[test]
fn exit_reason_from_cause_renders_failures_as_data() {
  assert_eq!(
    ExitReason::from_cause(&Cause::fail("boom")),
    ExitReason::Error("\"boom\"".to_string())
  );
  assert_eq!(
    ExitReason::from_cause::<String>(&Cause::die("panicked")),
    ExitReason::Error("panicked".to_string())
  );
}

#[test]
fn error_types_render_human_messages() {
  assert_eq!(CallError::Timeout.to_string(), "call timed out");
  assert_eq!(
    CallError::Down(ExitReason::NoProc).to_string(),
    "callee exited: noproc"
  );
  assert_eq!(CallError::MailboxClosed.to_string(), "mailbox closed");
  assert!(SendError::Rejected.to_string().contains("rejected"));
  assert_eq!(
    id_effect_process::StartError::Failed("nope".into()).to_string(),
    "start failed: nope"
  );
}
