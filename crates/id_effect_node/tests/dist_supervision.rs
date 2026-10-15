//! DistributedSupervisor takeover on NodeDown (ADR 0009 § distributed
//! supervision). Deterministic over MeshFabric — no sockets.

use std::sync::Arc;

use id_effect::Never;
use id_effect::next::{Effect, succeed};
use id_effect_node::id::NodeId;
use id_effect_node::membership::{Member, MembershipEvent};
use id_effect_node::{
  BehaviorRegistry, DistChildSpec, DistributedSupervisor, Mesh, MeshFabric, RemoteFault,
};
use id_effect_process::{Next, NodeName, Process, ProcessCtx, ProcessNode};

struct Worker;

impl Process for Worker {
  type Args = ();
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = ();
  type Info = ();
  type Error = Never;

  fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
    succeed(Worker)
  }

  fn handle_call(self, _: (), _: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }
}

fn mesh_pair() -> (Arc<Mesh>, Arc<Mesh>) {
  let fabric = MeshFabric::new();
  let mut ra = BehaviorRegistry::new();
  ra.register::<Worker>("worker");
  let mut rb = BehaviorRegistry::new();
  rb.register::<Worker>("worker");
  let a = Mesh::new(
    NodeId::fresh(&NodeName::new("a@test")),
    ProcessNode::new(NodeName::new("a@test")),
    ra,
  )
  .with_fabric(fabric.clone());
  let b = Mesh::new(
    NodeId::fresh(&NodeName::new("b@test")),
    ProcessNode::new(NodeName::new("b@test")),
    rb,
  )
  .with_fabric(fabric);
  (a, b)
}

fn mesh_trio() -> (Arc<Mesh>, Arc<Mesh>, Arc<Mesh>) {
  let fabric = MeshFabric::new();
  let make = |name: &str| {
    let mut r = BehaviorRegistry::new();
    r.register::<Worker>("worker");
    Mesh::new(
      NodeId::fresh(&NodeName::new(name)),
      ProcessNode::new(NodeName::new(name)),
      r,
    )
    .with_fabric(fabric.clone())
  };
  (make("a@test"), make("b@test"), make("c@test"))
}

fn spec() -> DistChildSpec {
  DistChildSpec {
    id: "w1".into(),
    behavior: "worker".into(),
    args: postcard::to_allocvec(&()).unwrap(),
    prefer: Some("b@test".into()),
  }
}

fn spec_anywhere() -> DistChildSpec {
  DistChildSpec {
    id: "w1".into(),
    behavior: "worker".into(),
    args: postcard::to_allocvec(&()).unwrap(),
    prefer: None,
  }
}

#[test]
fn child_starts_on_preferred_node() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let pid = sup.start_child(spec(), None).expect("start");
  assert_eq!(pid.node().as_str(), "b@test");
  assert_eq!(sup.len(), 1);
  let listed = sup.list();
  assert_eq!(listed[0].2, "b@test");
}

#[test]
fn takeover_replaces_child_on_node_down() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let original = sup.start_child(spec(), None).expect("start");
  assert_eq!(original.node().as_str(), "b@test");

  // b departs the cluster.
  let down = Member::new(NodeId::fresh(&NodeName::new("b@test")), "b@test");
  // Takeover is synchronous: no waiting needed before inspecting it.
  sup.on_membership(&MembershipEvent::NodeDown(down));

  // Exactly one child, now hosted locally (Horde-style takeover on self).
  assert_eq!(sup.len(), 1, "child re-placed, not duplicated");
  let listed = sup.list();
  assert_eq!(listed[0].0, "w1", "same logical child id");
  assert_eq!(listed[0].2, "a@test", "took over on surviving node");
  assert_eq!(listed[0].1.node().as_str(), "a@test");
}

#[test]
fn placement_picks_least_loaded_node() {
  let (a, _b, _c) = mesh_trio();
  // Skewed telemetry: a busy, c mid, b idle (most policy headroom).
  a.report_load("a@test", 0.9, 0.9);
  a.report_load("b@test", 0.1, 0.1);
  a.report_load("c@test", 0.5, 0.5);

  let sup = DistributedSupervisor::new(Arc::clone(&a));
  // No `prefer`: the unified placement API decides purely on load.
  let pid = sup.start_child(spec_anywhere(), None).expect("start");

  assert_eq!(
    pid.node().as_str(),
    "b@test",
    "least-loaded eligible node wins under LocalFirst"
  );
  assert_eq!(sup.len(), 1);
  assert_eq!(sup.list()[0].2, "b@test");
}

#[test]
fn node_down_for_unrelated_node_is_noop() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  sup.start_child(spec(), None).expect("start");
  let down = Member::new(NodeId::fresh(&NodeName::new("z@test")), "z@test");
  sup.on_membership(&MembershipEvent::NodeDown(down));
  let listed = sup.list();
  assert_eq!(listed.len(), 1);
  assert_eq!(
    listed[0].2, "b@test",
    "unrelated NodeDown must not move child"
  );
}

fn down(name: &str) -> MembershipEvent {
  MembershipEvent::NodeDown(Member::new(NodeId::fresh(&NodeName::new(name)), name))
}

fn spec_with(id: &str, prefer: Option<&str>) -> DistChildSpec {
  DistChildSpec {
    id: id.into(),
    behavior: "worker".into(),
    args: postcard::to_allocvec(&()).unwrap(),
    prefer: prefer.map(Into::into),
  }
}

#[test]
fn new_supervisor_is_empty() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(a);
  assert!(sup.is_empty());
  assert_eq!(sup.len(), 0);
  assert!(sup.list().is_empty());
}

#[test]
fn pinned_child_ignores_prefer_and_placement() {
  let (a, _b) = mesh_pair();
  a.report_load("b@test", 0.0, 0.0);
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let pid = sup
    .start_child(spec_with("w", Some("b@test")), Some("a@test"))
    .expect("pinned");
  assert_eq!(pid.node().as_str(), "a@test");
  assert_eq!(sup.list()[0].2, "a@test");
}

#[test]
fn pinning_to_an_unknown_node_fails_and_records_nothing() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(a);
  assert!(
    sup
      .start_child(spec_with("w", None), Some("z@test"))
      .is_err()
  );
  assert!(sup.is_empty());
}

#[test]
fn unknown_behavior_is_not_recorded() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(a);
  let mut spec = spec_with("w", None);
  spec.behavior = "nope".into();
  assert!(sup.start_child(spec, None).is_err());
  assert!(sup.is_empty());
}

#[test]
fn restarting_a_child_id_replaces_it() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let first = sup.start_child(spec_with("w", None), None).expect("first");
  let second = sup
    .start_child(spec_with("w", Some("b@test")), None)
    .expect("restart");
  assert_ne!(first, second);
  assert_eq!(sup.len(), 1);
  assert_eq!(sup.list()[0].1, second);
}

#[test]
fn takeover_uses_telemetry_and_skips_the_downed_node() {
  let (a, _b, _c) = mesh_trio();
  a.report_load("a@test", 0.9, 0.9);
  a.report_load("b@test", 0.1, 0.1);
  a.report_load("c@test", 0.5, 0.5);
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let pid = sup.start_child(spec_anywhere(), None).expect("start");
  assert_eq!(pid.node().as_str(), "b@test");

  sup.on_membership(&down("b@test"));

  let listed = sup.list();
  assert_eq!(listed.len(), 1);
  assert_eq!(
    listed[0].2, "c@test",
    "least-loaded survivor, never the downed node"
  );
}

#[test]
fn only_children_of_the_downed_node_move() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let local = sup
    .start_child(spec_with("local", None), Some("a@test"))
    .expect("local");
  sup
    .start_child(spec_with("remote", None), Some("b@test"))
    .expect("remote");

  sup.on_membership(&down("b@test"));

  let mut listed = sup.list();
  listed.sort_by(|x, y| x.0.cmp(&y.0));
  assert_eq!(listed.len(), 2);
  assert_eq!(listed[0].0, "local");
  assert_eq!(listed[0].1, local, "untouched child keeps its pid");
  assert_eq!(listed[1].0, "remote");
  assert_eq!(listed[1].2, "a@test");
}

fn mesh_with(fabric: &MeshFabric, name: &str, hosts_worker: bool) -> Arc<Mesh> {
  let mut registry = BehaviorRegistry::new();
  if hosts_worker {
    registry.register::<Worker>("worker");
  }
  Mesh::new(
    NodeId::fresh(&NodeName::new(name)),
    ProcessNode::new(NodeName::new(name)),
    registry,
  )
  .with_fabric(fabric.clone())
}

fn up(name: &str) -> MembershipEvent {
  MembershipEvent::NodeUp(Member::new(NodeId::fresh(&NodeName::new(name)), name))
}

/// Regression: a `prefer` naming a missing node used to orphan the child
/// even though the local node could host it ("prefer OR local").
#[test]
fn unreachable_prefer_falls_back_to_the_local_node() {
  let fabric = MeshFabric::new();
  let a = mesh_with(&fabric, "a@test", true);
  let _b = mesh_with(&fabric, "b@test", true);
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  sup
    .start_child(spec_with("w", Some("c@test")), Some("b@test"))
    .expect("start on b");

  sup.on_membership(&down("b@test"));

  assert!(sup.orphaned().is_empty());
  assert_eq!(
    sup.list()[0].2,
    "a@test",
    "prefer c is gone, local hosts it"
  );
}

/// Regression: a failed takeover used to drop the child silently.
#[test]
fn failed_takeover_keeps_the_child_orphaned_until_a_node_joins() {
  let fabric = MeshFabric::new();
  // a supervises but cannot host workers; b hosts until it departs.
  let a = mesh_with(&fabric, "a@test", false);
  let _b = mesh_with(&fabric, "b@test", true);
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  sup
    .start_child(spec_with("w", Some("c@test")), Some("b@test"))
    .expect("start on b");

  // No node can host it: prefer c is absent and a lacks the behavior.
  sup.on_membership(&down("b@test"));
  assert!(sup.is_empty(), "nothing running");
  let orphaned = sup.orphaned();
  assert_eq!(orphaned.len(), 1, "tracked, not lost");
  assert_eq!(orphaned[0].0, "w");
  assert!(
    matches!(orphaned[0].1, RemoteFault::UnknownBehavior(_)),
    "last candidate tried was the local node"
  );
  assert_eq!(sup.retry_orphaned().len(), 1, "still nowhere to go");

  // c joins: the retry runs off the membership path and lands there.
  let _c = mesh_with(&fabric, "c@test", true);
  let retry = sup.on_membership(&up("c@test")).expect("retry started");
  assert!(retry.join().expect("retry thread").is_empty());
  assert!(sup.orphaned().is_empty());
  let listed = sup.list();
  assert_eq!(listed.len(), 1);
  assert_eq!(listed[0].0, "w");
  assert_eq!(listed[0].2, "c@test");
}

#[test]
fn retries_avoid_the_departed_node_until_it_rejoins() {
  let fabric = MeshFabric::new();
  let a = mesh_with(&fabric, "a@test", false);
  let _b = mesh_with(&fabric, "b@test", true);
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  sup
    .start_child(spec_with("w", None), Some("b@test"))
    .expect("start on b");
  sup.on_membership(&down("b@test"));
  assert_eq!(sup.orphaned().len(), 1);

  // Stale telemetry still advertises b; a retry must not go back there.
  a.report_load("b@test", 0.1, 0.1);
  assert_eq!(sup.retry_orphaned().len(), 1);
  let other = sup.on_membership(&up("z@test")).expect("retry started");
  assert_eq!(other.join().expect("retry thread").len(), 1);
  assert!(sup.is_empty());

  // b itself rejoining makes it eligible again.
  let back = sup.on_membership(&up("b@test")).expect("retry started");
  assert!(back.join().expect("retry thread").is_empty());
  assert_eq!(sup.list()[0].2, "b@test");
}

#[test]
fn explicit_restart_clears_an_orphan() {
  let fabric = MeshFabric::new();
  let a = mesh_with(&fabric, "a@test", false);
  let _b = mesh_with(&fabric, "b@test", true);
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  sup
    .start_child(spec_with("w", None), Some("b@test"))
    .expect("start on b");
  sup.on_membership(&down("b@test"));
  assert_eq!(sup.orphaned().len(), 1);

  sup
    .start_child(spec_with("w", None), Some("b@test"))
    .expect("operator pins it back");
  assert!(sup.orphaned().is_empty());
  assert_eq!(sup.len(), 1);
}

#[test]
fn non_down_events_are_ignored_without_orphans() {
  let (a, _b) = mesh_pair();
  let sup = DistributedSupervisor::new(Arc::clone(&a));
  let pid = sup.start_child(spec(), None).expect("start");
  let member = Member::new(NodeId::fresh(&NodeName::new("b@test")), "b@test");
  assert!(
    sup
      .on_membership(&MembershipEvent::NodeUp(member.clone()))
      .is_none()
  );
  assert!(
    sup
      .on_membership(&MembershipEvent::Rejoined(member))
      .is_none()
  );
  assert_eq!(sup.list()[0].1, pid);
}
