//! Cluster-wide name registry (dist.placement#5): names resolve from every node, are released
//! when their process exits or their node goes down, and conflicting claims made on both sides
//! of a partition resolve to one owner when it heals. Deterministic: MeshFabric and the
//! partitionable peer senders deliver synchronously, and exits are awaited — no sleeps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use id_effect::{Cause, Exit, Never};
use id_effect::{Effect, succeed, sync};
use id_effect_node::envelope::RemoteFault;
use id_effect_node::{
  BehaviorRegistry, GlobalNameError, Mesh, MeshFabric, NodeId, conflicting_name,
};
use id_effect_process::{ExitReason, Next, NodeName, Pid, Process, ProcessCtx, ProcessNode};
use serde::{Deserialize, Serialize};

const WAIT: Duration = Duration::from_secs(5);

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

fn mesh_with(id: NodeId) -> Mesh {
  let name = NodeName::new(id.name());
  Mesh::new(id, ProcessNode::new(name), registry())
}

fn on_fabric(fabric: &MeshFabric, name: &str) -> Arc<Mesh> {
  mesh_with(NodeId::fresh(&NodeName::new(name))).with_fabric(fabric.clone())
}

fn spawn(mesh: &Mesh) -> Pid {
  let args = postcard::to_allocvec(&()).expect("unit args");
  mesh.spawn_local("counter", &args, None).expect("spawn")
}

fn add(n: u32) -> Vec<u8> {
  postcard::to_allocvec(&CounterCast::Add(n)).expect("encode")
}

fn get_by_name(mesh: &Mesh, name: &str) -> u32 {
  let call = postcard::to_allocvec(&CounterCall::Get).expect("encode");
  let reply = run(mesh.global().call(name, call, WAIT)).expect("call by name");
  postcard::from_bytes(&reply).expect("decode reply")
}

/// Waits for `pid` to exit and be finalized. Built before the exit is triggered: the effect
/// holds the process cell, so it cannot miss an exit that finalizes early.
fn exit_of(node: &ProcessNode, pid: &Pid) -> impl FnOnce() -> ExitReason {
  let exited: Effect<ExitReason, Never> = node.await_exit(pid);
  move || match exited.block_on() {
    Exit::Success(reason) => reason,
    Exit::Failure(c) => panic!("await_exit failed: {c:?}"),
  }
}

/// Connect `from` → `to` with a sender that drops everything while `cut` is set.
fn connect(from: &Mesh, from_id: &NodeId, to: &Mesh, cut: &Arc<AtomicBool>) {
  let (to, from_id, cut) = (to.clone(), from_id.clone(), Arc::clone(cut));
  let name = to.node_name().to_string();
  from.add_peer(
    name,
    Arc::new(move |envelope| {
      if cut.load(Ordering::SeqCst) {
        Err(RemoteFault::NoProc)
      } else {
        to.handle_envelope_from(&from_id, envelope)
      }
    }),
  );
}

/// Two meshes joined by partitionable links (initially cut).
fn partitioned(a_id: NodeId, b_id: NodeId) -> (Mesh, Mesh, Arc<AtomicBool>) {
  let a = mesh_with(a_id.clone());
  let b = mesh_with(b_id.clone());
  let cut = Arc::new(AtomicBool::new(true));
  connect(&a, &a_id, &b, &cut);
  connect(&b, &b_id, &a, &cut);
  (a, b, cut)
}

#[test]
fn global_name_resolves_from_every_node() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let c = on_fabric(&fabric, "c@test");
  let pid = spawn(&b);
  b.global().register("svc", &pid).expect("register");

  for mesh in [&a, &b, &c] {
    assert_eq!(
      mesh.global().whereis("svc"),
      Some(pid.clone()),
      "{}",
      mesh.node_name()
    );
    assert_eq!(
      mesh.global().names(),
      vec![("svc".to_string(), pid.clone())]
    );
  }
  a.global().cast("svc", add(3)).expect("cast by name");
  c.global().cast("svc", add(4)).expect("cast by name");
  for mesh in [&a, &b, &c] {
    assert_eq!(get_by_name(mesh, "svc"), 7);
  }
  assert_eq!(a.global().whereis("nobody"), None);
  assert_eq!(a.global().cast("nobody", add(1)), Err(RemoteFault::NoProc));
}

#[test]
fn addr_is_obtained_by_global_name() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let addr = b.process().spawn::<Counter>((), ());
  b.registry()
    .adopt_local::<Counter>("counter", addr.pid(), addr.clone_mailbox());
  b.global().register_addr("box", &addr).expect("register");

  let found = b
    .global()
    .whereis_addr::<Counter>("box")
    .expect("typed addr on the owner node");
  assert_eq!(found.pid(), addr.pid());
  let cast: Effect<(), _> = found.cast(CounterCast::Add(2));
  assert!(matches!(cast.block_on(), Exit::Success(())));
  let reply: Effect<u32, _> = found.call(CounterCall::Get, WAIT);
  assert!(matches!(reply.block_on(), Exit::Success(2)));
  // Other nodes resolve the pid and reach the process through the mesh.
  assert!(a.global().whereis_addr::<Counter>("box").is_none());
  assert_eq!(a.global().whereis("box").as_ref(), Some(addr.pid()));
  assert_eq!(get_by_name(&a, "box"), 2);
}

#[test]
fn name_released_on_process_exit() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let pid = spawn(&b);
  b.global().register("svc", &pid).expect("register");
  assert_eq!(a.global().whereis("svc"), Some(pid.clone()));

  let exited = exit_of(b.process(), &pid);
  b.process().exit(&pid, ExitReason::Killed);
  assert_eq!(exited(), ExitReason::Killed);

  assert_eq!(b.global().whereis("svc"), None);
  assert_eq!(a.global().whereis("svc"), None);
  // The name is free again, cluster-wide.
  let next = spawn(&a);
  a.global().register("svc", &next).expect("re-register");
  assert_eq!(b.global().whereis("svc"), Some(next));
}

#[test]
fn name_released_on_node_down() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let c = on_fabric(&fabric, "c@test");
  let pid = spawn(&b);
  b.global().register("svc", &pid).expect("register");

  a.peer_down("b@test");
  assert_eq!(a.global().whereis("svc"), None);
  assert!(a.global().names().is_empty());
  // Only the node that saw b go down forgets it.
  assert_eq!(c.global().whereis("svc"), Some(pid));
  let local = spawn(&a);
  a.global().register("svc", &local).expect("name free on a");
}

#[test]
fn register_twice_fails_with_already_registered() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let (first, second) = (spawn(&b), spawn(&b));
  b.global().register("svc", &first).expect("register");

  let taken = GlobalNameError::AlreadyRegistered {
    name: "svc".to_string(),
    owner: first.clone(),
  };
  assert_eq!(b.global().register("svc", &second), Err(taken.clone()));
  assert_eq!(b.global().register("svc", &first), Err(taken.clone()));
  // Another node's replica knows the name is taken too.
  let elsewhere = spawn(&a);
  assert_eq!(a.global().register("svc", &elsewhere), Err(taken));
  assert_eq!(b.global().whereis("svc"), Some(first.clone()));

  // Only the owner node releases a name.
  assert_eq!(
    a.global().unregister("svc"),
    Err(GlobalNameError::NotLocal(first))
  );
  assert_eq!(b.global().unregister("svc"), Ok(true));
  assert_eq!(b.global().unregister("svc"), Ok(false));
  assert_eq!(a.global().whereis("svc"), None);
  a.global().register("svc", &elsewhere).expect("free again");
  assert_eq!(b.global().whereis("svc"), Some(elsewhere));
}

#[test]
fn register_refuses_remote_dead_and_invalid() {
  let fabric = MeshFabric::new();
  let a = on_fabric(&fabric, "a@test");
  let b = on_fabric(&fabric, "b@test");
  let remote = spawn(&b);
  assert_eq!(
    a.global().register("svc", &remote),
    Err(GlobalNameError::NotLocal(remote))
  );

  let dead = spawn(&a);
  let exited = exit_of(a.process(), &dead);
  a.process().exit(&dead, ExitReason::Killed);
  exited();
  assert_eq!(
    a.global().register("svc", &dead),
    Err(GlobalNameError::NoProc(dead))
  );
  assert_eq!(a.global().whereis("svc"), None);
  assert_eq!(b.global().whereis("svc"), None);

  let live = spawn(&a);
  assert_eq!(
    a.global().register("", &live),
    Err(GlobalNameError::InvalidName)
  );
  assert_eq!(
    a.global()
      .register("x".repeat(id_effect_node::MAX_NAME_LEN + 1), &live),
    Err(GlobalNameError::InvalidName)
  );
}

#[test]
fn conflicting_registration_after_heal_keeps_one() {
  // b booted first (lower incarnation), so b keeps the name, whatever the alphabet says.
  let (a_id, b_id) = (
    NodeId::from_parts("a@test", 2),
    NodeId::from_parts("b@test", 1),
  );
  let (a, b, cut) = partitioned(a_id, b_id);
  let (on_a, on_b) = (spawn(&a), spawn(&b));
  a.global().register("svc", &on_a).expect("a side claims");
  b.global().register("svc", &on_b).expect("b side claims");
  assert_eq!(a.global().whereis("svc"), Some(on_a.clone()));
  assert_eq!(b.global().whereis("svc"), Some(on_b.clone()));
  let loser_exit = exit_of(a.process(), &on_a);

  cut.store(false, Ordering::SeqCst);
  a.global().sync_with("b@test").expect("sync a → b");
  b.global().sync_with("a@test").expect("sync b → a");

  for mesh in [&a, &b] {
    assert_eq!(
      mesh.global().whereis("svc"),
      Some(on_b.clone()),
      "{}",
      mesh.node_name()
    );
  }
  let reason = loser_exit();
  assert_eq!(conflicting_name(&reason), Some("svc"), "{reason:?}");
  // The winner is untouched, and the loser's exit released nothing it did not hold.
  assert_eq!(a.global().whereis("svc"), Some(on_b.clone()));
  assert_eq!(get_by_name(&b, "svc"), 0);
  assert!(a.global().whereis_addr::<Counter>("svc").is_none());
}

#[test]
fn heal_in_either_order_picks_the_same_owner() {
  for a_first in [true, false] {
    let (a_id, b_id) = (
      NodeId::from_parts("a@test", 5),
      NodeId::from_parts("b@test", 9),
    );
    let (a, b, cut) = partitioned(a_id, b_id);
    let (on_a, on_b) = (spawn(&a), spawn(&b));
    a.global().register("svc", &on_a).expect("claim");
    b.global().register("svc", &on_b).expect("claim");
    let loser_exit = exit_of(b.process(), &on_b);
    cut.store(false, Ordering::SeqCst);
    if a_first {
      a.global().sync_with("b@test").expect("sync");
      b.global().sync_with("a@test").expect("sync");
    } else {
      b.global().sync_with("a@test").expect("sync");
      a.global().sync_with("b@test").expect("sync");
    }
    assert_eq!(a.global().whereis("svc"), Some(on_a.clone()));
    assert_eq!(b.global().whereis("svc"), Some(on_a.clone()));
    assert_eq!(conflicting_name(&loser_exit()), Some("svc"));
  }
}

#[test]
fn release_during_partition_is_repaired_on_sync() {
  let (a, b, cut) = partitioned(
    NodeId::from_parts("a@test", 1),
    NodeId::from_parts("b@test", 2),
  );
  cut.store(false, Ordering::SeqCst);
  let pid = spawn(&a);
  a.global().register("svc", &pid).expect("register");
  assert_eq!(b.global().whereis("svc"), Some(pid));

  cut.store(true, Ordering::SeqCst);
  assert_eq!(a.global().unregister("svc"), Ok(true));
  assert!(b.global().whereis("svc").is_some(), "release lost");

  cut.store(false, Ordering::SeqCst);
  a.global().sync_with("b@test").expect("sync");
  assert_eq!(b.global().whereis("svc"), None);
}

#[test]
fn a_new_peer_learns_the_names_on_add_peer() {
  let (a_id, b_id) = (
    NodeId::from_parts("a@test", 1),
    NodeId::from_parts("b@test", 2),
  );
  let a = mesh_with(a_id.clone());
  let b = mesh_with(b_id.clone());
  let pid = spawn(&a);
  a.global().register("svc", &pid).expect("register alone");
  let cut = Arc::new(AtomicBool::new(false));
  connect(&a, &a_id, &b, &cut);
  assert_eq!(b.global().whereis("svc"), Some(pid));
}

#[test]
fn a_peer_cannot_claim_names_for_another_node() {
  let (a, b, cut) = partitioned(
    NodeId::from_parts("a@test", 1),
    NodeId::from_parts("b@test", 2),
  );
  cut.store(false, Ordering::SeqCst);
  let on_a = spawn(&a);
  a.global().register("svc", &on_a).expect("register");
  // b forwards a claim it does not own (a pid on c): a drops it.
  let forged = id_effect_node::NameClaim {
    name: "other".to_string(),
    pid: id_effect_node::PidWire {
      node: "c@test".to_string(),
      id: 1,
      incarnation: 1,
    },
    owner: NodeId::from_parts("c@test", 0),
  };
  a.handle_envelope_from(
    &NodeId::from_parts("b@test", 2),
    id_effect_node::ProcessEnvelope::GlobalRegister { claim: forged },
  )
  .expect("handled");
  assert_eq!(a.global().whereis("other"), None);
  // Nor release a name held by another node.
  a.handle_envelope_from(
    &NodeId::from_parts("b@test", 2),
    id_effect_node::ProcessEnvelope::GlobalUnregister {
      name: "svc".to_string(),
      pid: id_effect_node::PidWire::from(&on_a),
    },
  )
  .expect("handled");
  assert_eq!(a.global().whereis("svc"), Some(on_a));
  let _ = b;
}
