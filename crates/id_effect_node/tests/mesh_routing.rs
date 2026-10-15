//! Mesh routing semantics: replies reach only the caller, monitors fire
//! exactly once (including for unreachable nodes), explicit peer senders,
//! session-framed delivery, and group fanout. Deterministic: MeshFabric
//! delivery is synchronous, and asynchronous effects are awaited on
//! channels — no sleeps.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::next::{Effect, succeed, sync};
use id_effect::{Cause, Exit, Never};
use id_effect_node::envelope::{PidWire, ProcessEnvelope, RemoteFault};
use id_effect_node::wire::Frame;
use id_effect_node::{
  BehaviorRegistry, ClusterResourcePolicy, Cookie, MemoryNetwork, Mesh, MeshFabric, NodeId,
  PeerSender, ProcessGroups, Session, Transport, fanout_cast, fanout_cast_on_node,
};
use id_effect_process::{ExitReason, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode};
use serde::{Deserialize, Serialize};

const WAIT: Duration = Duration::from_secs(5);

/// Run a mesh request on the default runtime.
fn run<A: Send + 'static>(eff: Effect<A, RemoteFault>) -> Result<A, RemoteFault> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(f)) => Err(f),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

#[derive(Serialize, Deserialize)]
enum CounterCall {
  Get,
}

#[derive(Serialize, Deserialize)]
enum CounterCast {
  Add(u32),
}

struct Counter {
  n: u32,
}

impl Process for Counter {
  type Args = ();
  type Env = ();
  type Call = CounterCall;
  type Reply = u32;
  type Cast = CounterCast;
  type Info = ();
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Counter { n: 0 })
  }

  fn handle_call(self, msg: CounterCall, _: ProcessCtx) -> Effect<(u32, Next<Self>), Never, ()> {
    sync(move || match msg {
      CounterCall::Get => (self.n, Next::Continue(self)),
    })
  }

  fn handle_cast(mut self, msg: CounterCast, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || match msg {
      CounterCast::Add(x) => {
        self.n += x;
        Next::Continue(self)
      }
    })
  }
}

fn registry() -> BehaviorRegistry {
  let mut registry = BehaviorRegistry::new();
  registry.register::<Counter>("counter");
  registry
}

fn mesh(name: &str) -> Mesh {
  Mesh::new(
    NodeId::fresh(&NodeName::new(name)),
    ProcessNode::new(NodeName::new(name)),
    registry(),
  )
}

fn on_fabric(fabric: &MeshFabric, name: &str) -> Arc<Mesh> {
  mesh(name).with_fabric(fabric.clone())
}

fn unit() -> Vec<u8> {
  postcard::to_allocvec(&()).expect("unit args")
}

fn get(mesh: &Mesh, pid: &Pid) -> u32 {
  let call = postcard::to_allocvec(&CounterCall::Get).expect("encode");
  let reply = run(mesh.remote_call(pid, call, WAIT)).expect("call");
  postcard::from_bytes(&reply).expect("decode reply")
}

fn add(n: u32) -> Vec<u8> {
  postcard::to_allocvec(&CounterCast::Add(n)).expect("encode")
}

/// A peer sender that records every envelope and reports it on a channel.
fn recorder() -> (PeerSender, flume::Receiver<ProcessEnvelope>) {
  let (tx, rx) = flume::unbounded();
  let sender: PeerSender = Arc::new(move |envelope| {
    let _ = tx.send(envelope);
    Ok(())
  });
  (sender, rx)
}

#[test]
fn cast_then_call_is_ordered_without_waiting() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let pid = run(a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("spawn");
  a.remote_cast(&pid, add(3)).expect("cast");
  a.remote_cast(&pid, add(4)).expect("cast");
  // The mailbox is FIFO: the call is handled after both casts.
  assert_eq!(get(&a, &pid), 7);
}

#[test]
fn spawning_on_self_short_circuits_to_a_local_spawn() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let pid = run(a.remote_spawn("a@test", "counter", unit(), None, WAIT)).expect("local spawn");
  assert_eq!(pid.node().as_str(), "a@test");
  assert!(a.registry().has_route(&pid));
  a.remote_cast(&pid, add(2)).expect("local cast");
  assert_eq!(get(&a, &pid), 2);
}

#[test]
fn call_to_a_missing_remote_pid_is_noproc() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let ghost = Pid::from_parts(NodeName::new("b@test"), 9_999, 1);
  let call = postcard::to_allocvec(&CounterCall::Get).expect("encode");
  assert_eq!(
    run(a.remote_call(&ghost, call, WAIT)),
    Err(RemoteFault::NoProc)
  );
}

#[test]
fn cast_to_a_missing_local_pid_is_noproc() {
  let a = mesh("a@test");
  let ghost = Pid::from_parts(NodeName::new("a@test"), 9_999, 1);
  assert_eq!(a.remote_cast(&ghost, add(1)), Err(RemoteFault::NoProc));
  assert_eq!(a.remote_info(&ghost, unit()), Err(RemoteFault::NoProc));
}

#[test]
fn unreachable_node_is_noproc_for_every_operation() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let elsewhere = Pid::from_parts(NodeName::new("z@test"), 1, 1);
  assert_eq!(a.remote_cast(&elsewhere, add(1)), Err(RemoteFault::NoProc));
  assert_eq!(
    run(a.remote_spawn("z@test", "counter", unit(), None, WAIT)),
    Err(RemoteFault::NoProc)
  );
}

#[test]
fn undecodable_cast_payload_is_a_decode_fault() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let pid = run(a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("spawn");
  assert!(matches!(
    a.remote_cast(&pid, vec![0xff, 0xff]),
    Err(RemoteFault::Decode(_))
  ));
}

/// Regression: replies used to be broadcast to every peer, so a reply to
/// `a`'s spawn #1 also resolved `c`'s unrelated pending spawn #1.
#[test]
fn spawn_replies_reach_only_the_caller() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let c = on_fabric(&fabric, "c@test");

  // c's spawn goes to a black hole that never answers.
  let (sink, sent) = recorder();
  c.add_peer("z@test", sink);
  let c_spawn = {
    let c = Arc::clone(&c);
    std::thread::spawn(move || {
      run(c.remote_spawn(
        "z@test",
        "counter",
        unit(),
        None,
        Duration::from_millis(300),
      ))
    })
  };
  // Once z has the request, c's pending spawn #1 is registered.
  assert!(matches!(
    sent.recv_timeout(WAIT).expect("c sent its spawn"),
    ProcessEnvelope::Spawn { spawn_id: 1, .. }
  ));

  // a's spawn #1 on b succeeds; its reply must not leak to c.
  let pid = run(a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("a spawn");
  assert_eq!(pid.node().as_str(), "b@test");
  assert_eq!(
    c_spawn.join().expect("join"),
    Err(RemoteFault::Timeout),
    "c's spawn was never answered"
  );
}

#[test]
fn monitoring_an_unreachable_node_fires_noconnection_at_once() {
  let a = mesh("a@test");
  let seen = Arc::new(Mutex::new(Vec::new()));
  let log = Arc::clone(&seen);
  let target = Pid::from_parts(NodeName::new("z@test"), 1, 1);
  a.remote_monitor(
    &target,
    Box::new(move |_, pid, reason| log.lock().expect("log").push((pid, reason))),
  );
  assert_eq!(
    *seen.lock().expect("log"),
    vec![(target, ExitReason::NoConnection)]
  );
  // The monitor is gone: a later peer_down must not fire it again.
  a.peer_down("z@test");
  assert_eq!(seen.lock().expect("log").len(), 1);
}

#[test]
fn monitoring_a_dead_remote_pid_fires_noproc() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let (tx, rx) = flume::unbounded();
  let ghost = Pid::from_parts(NodeName::new("b@test"), 9_999, 1);
  a.remote_monitor(
    &ghost,
    Box::new(move |_, _, reason| {
      let _ = tx.send(reason);
    }),
  );
  assert_eq!(rx.recv_timeout(WAIT).expect("down"), ExitReason::NoProc);
}

#[test]
fn remote_monitor_fires_when_the_target_exits() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let pid = run(a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("spawn");
  let (tx, rx) = flume::unbounded();
  a.remote_monitor(
    &pid,
    Box::new(move |_, who, reason| {
      let _ = tx.send((who, reason));
    }),
  );
  b.process().exit(&pid, ExitReason::Killed);
  let (who, reason) = rx.recv_timeout(WAIT).expect("down");
  assert_eq!(who, pid);
  assert_eq!(reason, ExitReason::Killed);
}

/// Regression: `Demonitor` used the caller's reference on the target node,
/// where it matched nothing, so the monitor stayed armed.
#[test]
fn demonitored_remote_monitor_never_fires() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let pid = run(a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("spawn");
  let (cancelled_tx, cancelled_rx) = flume::unbounded();
  let cancelled = a.remote_monitor(
    &pid,
    Box::new(move |_, _, reason| {
      let _ = cancelled_tx.send(reason);
    }),
  );
  // A second monitor is the sentinel that tells us the exit was processed.
  let (sentinel_tx, sentinel_rx) = flume::unbounded();
  a.remote_monitor(
    &pid,
    Box::new(move |_, _, reason| {
      let _ = sentinel_tx.send(reason);
    }),
  );

  a.remote_demonitor(&pid, cancelled);
  a.remote_demonitor(&pid, cancelled); // idempotent
  b.process().exit(&pid, ExitReason::Killed);

  assert_eq!(
    sentinel_rx.recv_timeout(WAIT).expect("sentinel down"),
    ExitReason::Killed
  );
  assert!(cancelled_rx.try_recv().is_err(), "demonitored sink fired");
}

#[test]
fn local_demonitor_cancels_a_local_monitor() {
  let a = mesh("a@test");
  let pid = run(a.remote_spawn("a@test", "counter", unit(), None, WAIT)).expect("spawn");
  let (tx, rx) = flume::unbounded();
  let monitor = a.remote_monitor(
    &pid,
    Box::new(move |_, _, reason| {
      let _ = tx.send(reason);
    }),
  );
  let (sentinel_tx, sentinel_rx) = flume::unbounded();
  a.remote_monitor(
    &pid,
    Box::new(move |_, _, reason| {
      let _ = sentinel_tx.send(reason);
    }),
  );
  a.remote_demonitor(&pid, monitor);
  a.process().exit(&pid, ExitReason::Killed);
  sentinel_rx.recv_timeout(WAIT).expect("sentinel down");
  assert!(rx.try_recv().is_err());
}

#[test]
fn explicit_peer_sender_takes_precedence_over_the_fabric() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let (sink, sent) = recorder();
  a.add_peer("b@test", sink);
  let pid = Pid::from_parts(NodeName::new("b@test"), 1, 1);
  a.remote_cast(&pid, add(1)).expect("cast");
  assert!(matches!(
    sent.try_recv().expect("routed via sender"),
    ProcessEnvelope::Cast { to, .. } if to == PidWire::from(&pid)
  ));
}

#[test]
fn peer_down_forgets_the_peer_sender() {
  let a = mesh("a@test");
  let (sink, sent) = recorder();
  a.add_peer("z@test", sink);
  let pid = Pid::from_parts(NodeName::new("z@test"), 1, 1);
  a.remote_info(&pid, unit()).expect("delivered");
  assert!(sent.try_recv().is_ok());
  a.peer_down("z@test");
  assert_eq!(a.remote_info(&pid, unit()), Err(RemoteFault::NoProc));
}

#[test]
fn load_reports_keep_the_latest_value_per_node() {
  let a = mesh("a@test");
  a.report_load("b@test", 0.9, 0.9);
  a.report_load("b@test", 0.1, 0.2);
  let reports = a.load_reports();
  assert_eq!(reports.len(), 1);
  assert_eq!(reports[0].node, "b@test");
  assert!((reports[0].cpu_pct - 0.1).abs() < f32::EPSILON);
  assert!((reports[0].mem_pct - 0.2).abs() < f32::EPSILON);
}

#[test]
fn placed_spawn_without_eligible_nodes_is_noproc() {
  let a = mesh("a@test");
  let policy =
    ClusterResourcePolicy::local_first(id_effect::ResourcePolicy::memory_cap_max_cpu(1.0));
  assert_eq!(
    run(a.remote_spawn_placed(&[], &policy, "counter", unit(), None, WAIT)),
    Err(RemoteFault::NoProc)
  );
}

async fn session_pair() -> (Session, Session) {
  let net = MemoryNetwork::new();
  let listener = net.transport("b").listen("b").await.expect("bind");
  let dial = net.transport("a").connect("b").await.expect("dial");
  let accept = listener.accept().await.expect("accept");
  let cookie = Cookie::new("mesh");
  let (id_a, id_b) = (
    NodeId::from_parts("a@test", 1),
    NodeId::from_parts("b@test", 2),
  );
  let (a, b) = tokio::join!(
    Session::initiate(dial, &id_a, &cookie),
    Session::respond(accept, &id_b, &cookie),
  );
  (a.expect("a session"), b.expect("b session"))
}

#[tokio::test]
async fn session_frames_reply_to_the_authenticated_peer() {
  let (to_b, to_a) = session_pair().await;
  let b = mesh("b@test");
  let (sink, replies) = recorder();
  b.add_peer("a@test", sink);

  let spawn = ProcessEnvelope::Spawn {
    spawn_id: 7,
    behavior: "counter".to_string(),
    args: unit(),
    link_to: None,
  };
  Mesh::session_send(&to_b, &spawn).await.expect("send");
  b.handle_session_frame(&to_a).await.expect("handled");

  match replies.try_recv().expect("reply routed to a") {
    ProcessEnvelope::SpawnReply {
      spawn_id: 7,
      result: Ok(pid),
    } => assert_eq!(pid.node, "b@test"),
    other => panic!("unexpected reply {other:?}"),
  }
}

#[tokio::test]
async fn session_close_and_control_frames() {
  let (to_b, to_a) = session_pair().await;
  let b = mesh("b@test");
  to_b.send(&Frame::Ping(1)).await.expect("ping");
  b.handle_session_frame(&to_a)
    .await
    .expect("ping is ignored");
  to_b
    .send(&Frame::Close("bye".to_string()))
    .await
    .expect("close");
  assert_eq!(
    b.handle_session_frame(&to_a).await,
    Err(RemoteFault::NoProc)
  );
}

#[tokio::test]
async fn garbage_data_frame_is_a_decode_fault() {
  let (to_b, to_a) = session_pair().await;
  let b = mesh("b@test");
  to_b
    .send(&Frame::Data(vec![0xff, 0xff, 0xff]))
    .await
    .expect("send");
  assert!(matches!(
    b.handle_session_frame(&to_a).await,
    Err(RemoteFault::Decode(_))
  ));
}

#[tokio::test]
async fn dead_session_surfaces_as_a_fault() {
  let (to_b, to_a) = session_pair().await;
  let b = mesh("b@test");
  to_b.close().await;
  assert!(b.handle_session_frame(&to_a).await.is_err());
  assert!(
    Mesh::session_send(
      &to_b,
      &ProcessEnvelope::Reply {
        call_id: 1,
        result: Ok(Vec::new())
      }
    )
    .await
    .is_err()
  );
}

#[test]
fn fanout_to_an_empty_group_delivers_nothing() {
  let a = mesh("a@test");
  let groups = ProcessGroups::new("a@test");
  assert_eq!(fanout_cast(&a, &groups, "nobody", &add(1)), Ok(0));
}

#[test]
fn fanout_fails_only_when_every_member_fails() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let mut groups = ProcessGroups::new("a@test");
  groups.join("g", &Pid::from_parts(NodeName::new("z@test"), 1, 1));
  assert_eq!(
    fanout_cast(&a, &groups, "g", &add(1)),
    Err(RemoteFault::NoProc)
  );

  let live = run(a.remote_spawn("a@test", "counter", unit(), None, WAIT)).expect("spawn");
  groups.join("g", &live);
  assert_eq!(fanout_cast(&a, &groups, "g", &add(1)), Ok(1), "partial");
  assert_eq!(get(&a, &live), 1);
}

#[test]
fn fanout_on_node_targets_only_that_node() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let _b = on_fabric(&fabric, "b@test");
  let local = run(a.remote_spawn("a@test", "counter", unit(), None, WAIT)).expect("local");
  let remote = run(a.remote_spawn("b@test", "counter", unit(), None, WAIT)).expect("remote");
  let mut groups = ProcessGroups::new("a@test");
  groups.join("g", &local);
  groups.join("g", &remote);

  assert_eq!(
    fanout_cast_on_node(&a, &groups, "g", "b@test", &add(5)),
    Ok(1)
  );
  assert_eq!(get(&a, &remote), 5);
  assert_eq!(get(&a, &local), 0);
}
