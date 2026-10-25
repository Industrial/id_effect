//! The cross-node seam ([`Backplane`]) and its mesh implementation ([`MeshBackplane`]): one hub
//! process per node, topics as process groups, casts between hubs.
//!
//! The hub process registers under the cluster-wide name
//! `id_effect_rpc.channels/<namespace>/<node>` ([`GlobalRegistry`]); hubs find each other by that
//! prefix on every heartbeat, monitor each other (a node that goes down fires the monitor with
//! `noconnection`, and its members are dropped at once), and keep one [`ProcessGroups`] replica
//! each: a topic's group holds the hub pids of the nodes with subscribers on it. Interest changes
//! are gossiped as group snapshots (the ORSWOT merge is idempotent, so a lost or repeated
//! snapshot is harmless); a hub that learns something new answers with its own snapshot, so two
//! hubs converge in one round trip.
//!
//! [`GlobalRegistry`]: id_effect_node::GlobalRegistry

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use id_effect::{Effect, Never, succeed, sync};
use id_effect_node::{
  BehaviorRegistry, Cookie, Ingress, MemoryNetwork, Mesh, NodeId, OrSwot, ProcessEnvelope,
  ProcessGroups, RemoteFault, Session, SessionPump, SessionPumpStats, Transport,
};
use id_effect_process::{ExitReason, MonitorSink, Next, Pid, Process, ProcessCtx};
use serde::{Deserialize, Serialize};

use super::ChannelError;
use super::hub::HubSink;
use crate::transport::mesh::MeshRpc;

/// The behavior name of the hub process ([`register_hub`]).
pub const HUB_BEHAVIOR: &str = "id_effect_rpc.channels.hub";

/// Prefix of the hubs' cluster-wide names.
const GLOBAL_PREFIX: &str = "id_effect_rpc.channels/";

/// Encoded size above which group snapshots are split over several casts.
const CHUNK_BYTES: usize = 32 * 1024;

/// The most lost nodes remembered at once.
const MAX_LOST: usize = 1_024;

/// What the hub needs from the network. [`MeshBackplane`] is the mesh implementation; another
/// transport implements these five calls and hands what arrives to the [`Inbound`] it was
/// given.
pub trait Backplane: Send + Sync + 'static {
  /// This node now has (`true`) or no longer has subscribers on `topic`.
  fn set_interest(&self, topic: &str, interested: bool);
  /// Send `frame` to the hub of every other node interested in `topic`; how many were reached.
  fn fanout(&self, topic: &str, frame: &[u8]) -> usize;
  /// The other nodes interested in `topic`, as this node knows them.
  fn nodes(&self, topic: &str) -> Vec<String>;
  /// Periodic anti-entropy: find hubs, notice lost ones, re-send interest.
  fn heartbeat(&self);
  /// Stop: leave the network; other nodes see this node's hub go down.
  fn shutdown(&self);
}

/// Where a backplane delivers what arrives for this node's hub.
#[derive(Clone)]
pub struct Inbound {
  sink: Weak<dyn HubSink>,
}

impl fmt::Debug for Inbound {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Inbound")
      .field("live", &(self.sink.strong_count() > 0))
      .finish()
  }
}

impl Inbound {
  pub(crate) fn new(sink: Weak<dyn HubSink>) -> Self {
    Self { sink }
  }

  /// A frame another node's hub sent.
  pub fn frame(&self, bytes: &[u8]) {
    if let Some(sink) = self.sink.upgrade() {
      sink.frame(bytes);
    }
  }

  /// `node` went down (or its hub stopped): drop its members.
  pub fn node_down(&self, node: &str) {
    if let Some(sink) = self.sink.upgrade() {
      sink.node_down(node);
    }
  }

  /// `node`'s hub is reachable (again): accept its frames.
  pub fn node_up(&self, node: &str) {
    if let Some(sink) = self.sink.upgrade() {
      sink.node_up(node);
    }
  }

  /// The nodes in `alive` are the only ones with a live hub: members of any other node are
  /// dropped as if it went down.
  pub fn retain_nodes(&self, alive: &BTreeSet<String>) {
    if let Some(sink) = self.sink.upgrade() {
      sink.retain_nodes(alive);
    }
  }

  /// Another node became interested in `topic`: send it this node's members.
  pub fn peer_interest(&self, topic: &str) {
    if let Some(sink) = self.sink.upgrade() {
      sink.peer_interest(topic);
    }
  }
}

/// What hubs cast to each other.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum HubCast {
  /// A hub frame for the channels ([`Inbound::frame`]).
  Frame(Vec<u8>),
  /// Group snapshots from `node`'s replica.
  Groups {
    node: String,
    groups: Vec<(String, OrSwot)>,
  },
  /// `node` (re)discovered this hub: re-announce interest with fresh adds.
  Hello { node: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct HubArgs {
  slot: u64,
}

type CastSink = Arc<dyn Fn(HubCast) + Send + Sync>;

/// Sinks waiting for their hub process to start (the process arguments must be serializable,
/// so they carry a slot number, not the sink).
fn slots() -> &'static Mutex<HashMap<u64, CastSink>> {
  static SLOTS: OnceLock<Mutex<HashMap<u64, CastSink>>> = OnceLock::new();
  SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_SLOT: AtomicU64 = AtomicU64::new(1);

/// The hub process: hands every cast to its backplane.
pub(crate) struct HubProcess {
  sink: Option<CastSink>,
}

impl Process for HubProcess {
  type Args = HubArgs;
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = HubCast;
  type Info = ();
  type Error = Never;

  fn init(args: HubArgs, _: ProcessCtx) -> Effect<Self, Never, ()> {
    sync(move || HubProcess {
      // A hub spawned by a peer (`remote_spawn`) finds no slot and ignores everything.
      sink: slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&args.slot),
    })
  }

  fn handle_call(self, _: (), _: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
    succeed(((), Next::Continue(self)))
  }

  fn handle_cast(self, msg: HubCast, _: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
    sync(move || {
      if let Some(sink) = &self.sink {
        sink(msg);
      }
      Next::Continue(self)
    })
  }
}

/// Register the hub behavior in a node's registry. Every node running channels calls it before
/// it builds its [`Mesh`] (the mesh takes the registry by value).
pub fn register_hub(registry: &mut BehaviorRegistry) {
  registry.register::<HubProcess>(HUB_BEHAVIOR);
}

struct BpState {
  groups: ProcessGroups,
  interest: BTreeSet<String>,
  hubs: BTreeMap<String, Pid>,
  /// Nodes whose hub was lost and not found again: their casts still in flight are ignored.
  lost: BTreeSet<String>,
  stopped: bool,
}

/// The [`Backplane`] on the process mesh. See the module docs.
pub struct MeshBackplane {
  mesh: Mesh,
  name: String,
  prefix: String,
  hub: Pid,
  state: Mutex<BpState>,
  inbound: Inbound,
  me: Weak<MeshBackplane>,
}

impl fmt::Debug for MeshBackplane {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MeshBackplane")
      .field("name", &self.name)
      .field("hub", &self.hub)
      .finish_non_exhaustive()
  }
}

impl MeshBackplane {
  /// Spawn this node's hub for `namespace` on `mesh` and register it cluster-wide.
  ///
  /// # Errors
  ///
  /// [`ChannelError::Internal`] when [`register_hub`] was not called for the mesh's registry,
  /// the namespace is empty, longer than 64 bytes or holds `/`, or the namespace already has a
  /// hub on this node.
  pub fn start(mesh: &Mesh, namespace: &str, inbound: Inbound) -> Result<Arc<Self>, ChannelError> {
    if mesh.registry().get(HUB_BEHAVIOR).is_none() {
      return Err(ChannelError::Internal(
        "the hub behavior is not registered: call channels::register_hub before Mesh::new".into(),
      ));
    }
    if namespace.is_empty() || namespace.len() > 64 || namespace.contains('/') {
      return Err(ChannelError::Internal(format!(
        "invalid channels namespace {namespace:?}"
      )));
    }
    let prefix = format!("{GLOBAL_PREFIX}{namespace}/");
    let name = format!("{prefix}{}", mesh.node_name());
    if mesh.global().whereis(&name).is_some() {
      return Err(ChannelError::Internal(format!(
        "{name} already has a hub on this node"
      )));
    }
    let cell: Arc<OnceLock<Weak<MeshBackplane>>> = Arc::new(OnceLock::new());
    let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
    let target = Arc::clone(&cell);
    slots()
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(
        slot,
        Arc::new(move |cast| {
          if let Some(backplane) = target.get().and_then(Weak::upgrade) {
            backplane.on_cast(cast);
          }
        }),
      );
    let addr = mesh.process().spawn::<HubProcess>(HubArgs { slot }, ());
    mesh
      .registry()
      .adopt_local::<HubProcess>(HUB_BEHAVIOR, addr.pid(), addr.clone_mailbox());
    let backplane = Arc::new_cyclic(|me| Self {
      mesh: mesh.clone(),
      name: name.clone(),
      prefix,
      hub: addr.pid().clone(),
      state: Mutex::new(BpState {
        groups: ProcessGroups::new(mesh.node_name()),
        interest: BTreeSet::new(),
        hubs: BTreeMap::new(),
        lost: BTreeSet::new(),
        stopped: false,
      }),
      inbound,
      me: me.clone(),
    });
    let _ = cell.set(Arc::downgrade(&backplane));
    if let Err(e) = mesh.global().register_addr(name, &addr) {
      mesh.process().exit(addr.pid(), ExitReason::Shutdown);
      return Err(ChannelError::Internal(e.to_string()));
    }
    backplane.heartbeat();
    Ok(backplane)
  }

  /// This node's hub process.
  pub fn hub(&self) -> &Pid {
    &self.hub
  }

  /// The hubs of the other nodes this hub knows, by node name.
  pub fn hubs(&self) -> BTreeMap<String, Pid> {
    self.lock().hubs.clone()
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, BpState> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn node(&self) -> &str {
    self.mesh.node_name()
  }

  fn cast(&self, to: &Pid, msg: &HubCast) -> bool {
    match postcard::to_allocvec(msg) {
      Ok(bytes) => self.mesh.remote_cast(to, bytes).is_ok(),
      Err(_) => false,
    }
  }

  /// Send `groups` to `to`, split so that no cast is much larger than [`CHUNK_BYTES`].
  fn send_groups(&self, to: &Pid, groups: Vec<(String, OrSwot)>) {
    let mut chunk = Vec::new();
    let mut size = 0;
    for (topic, set) in groups {
      size += postcard::to_allocvec(&set).map_or(0, |b| b.len()) + topic.len();
      chunk.push((topic, set));
      if size >= CHUNK_BYTES {
        self.cast(to, &self.groups_cast(std::mem::take(&mut chunk)));
        size = 0;
      }
    }
    if !chunk.is_empty() {
      self.cast(to, &self.groups_cast(chunk));
    }
  }

  fn groups_cast(&self, groups: Vec<(String, OrSwot)>) -> HubCast {
    HubCast::Groups {
      node: self.node().to_string(),
      groups,
    }
  }

  fn snapshot(state: &BpState) -> Vec<(String, OrSwot)> {
    state
      .groups
      .group_names()
      .into_iter()
      .map(|g| {
        let set = state.groups.snapshot(&g);
        (g, set)
      })
      .collect()
  }

  /// Nodes other than this one in `topic`'s group.
  fn others(&self, state: &BpState, topic: &str) -> BTreeSet<String> {
    state
      .groups
      .members(topic)
      .into_iter()
      .map(|p| p.node().as_str().to_string())
      .filter(|n| n != self.node())
      .collect()
  }

  fn on_cast(&self, cast: HubCast) {
    match cast {
      HubCast::Frame(bytes) => self.inbound.frame(&bytes),
      HubCast::Groups { node, groups } => self.merge(&node, groups),
      HubCast::Hello { node } => self.hello(&node),
    }
  }

  /// Merge `from`'s snapshots; answer with ours when ours learned something, and tell the hub
  /// about nodes newly interested in topics it has subscribers on.
  fn merge(&self, from: &str, groups: Vec<(String, OrSwot)>) {
    let (answer, interested) = {
      let mut state = self.lock();
      if state.stopped || state.lost.contains(from) {
        return;
      }
      let mut changed = false;
      let mut interested = Vec::new();
      for (topic, set) in groups {
        let before = self.others(&state, &topic);
        state.groups.merge_group(&topic, &set);
        let after = self.others(&state, &topic);
        if after != before {
          changed = true;
          if state.interest.contains(&topic) && after.difference(&before).next().is_some() {
            interested.push(topic);
          }
        }
      }
      let answer = if changed {
        state
          .hubs
          .get(from)
          .cloned()
          .map(|hub| (hub, Self::snapshot(&state)))
      } else {
        None
      };
      (answer, interested)
    };
    if let Some((hub, snapshot)) = answer {
      self.send_groups(&hub, snapshot);
    }
    for topic in interested {
      self.inbound.peer_interest(&topic);
    }
  }

  /// `from` (re)discovered us: re-add our interest with fresh dots (a replica that dropped our
  /// entries while we were apart would otherwise keep them dropped) and send it to everyone.
  fn hello(&self, from: &str) {
    let (snapshot, hubs) = {
      let mut state = self.lock();
      if state.stopped || state.lost.contains(from) {
        return;
      }
      let topics: Vec<String> = state.interest.iter().cloned().collect();
      for topic in &topics {
        state.groups.leave(topic, &self.hub);
        state.groups.join(topic, &self.hub);
      }
      (
        Self::snapshot(&state),
        state.hubs.values().cloned().collect::<Vec<_>>(),
      )
    };
    tracing::debug!(node = %self.node(), %from, "channels: hello");
    for hub in hubs {
      self.send_groups(&hub, snapshot.clone());
    }
  }

  /// The hub on `node` (pid `hub`) is gone: forget it and its interest, drop its members.
  fn lost(&self, node: &str, hub: &Pid) {
    {
      let mut state = self.lock();
      if state.hubs.get(node) == Some(hub) {
        state.hubs.remove(node);
      }
      Self::forget_node(&mut state, node);
    }
    self.inbound.node_down(node);
  }

  fn forget_node(state: &mut BpState, node: &str) {
    if state.lost.len() >= MAX_LOST {
      state.lost.pop_first();
    }
    state.lost.insert(node.to_string());
    for group in state.groups.group_names() {
      for pid in state.groups.members(&group) {
        if pid.node().as_str() == node {
          state.groups.leave(&group, &pid);
        }
      }
    }
  }

  fn watch(&self, node: String, hub: Pid) {
    let me = self.me.clone();
    let watched = hub.clone();
    let sink: MonitorSink = Box::new(move |_, _, reason| {
      if let Some(backplane) = me.upgrade() {
        tracing::debug!(%node, ?reason, "channels: hub down");
        backplane.lost(&node, &watched);
      }
    });
    self.mesh.remote_monitor(&hub, sink);
  }
}

impl Backplane for MeshBackplane {
  fn set_interest(&self, topic: &str, interested: bool) {
    let (set, hubs) = {
      let mut state = self.lock();
      if state.stopped {
        return;
      }
      if interested {
        state.interest.insert(topic.to_string());
        state.groups.join(topic, &self.hub);
      } else {
        state.interest.remove(topic);
        state.groups.leave(topic, &self.hub);
      }
      (
        state.groups.snapshot(topic),
        state.hubs.values().cloned().collect::<Vec<_>>(),
      )
    };
    for hub in hubs {
      self.send_groups(&hub, vec![(topic.to_string(), set.clone())]);
    }
  }

  fn fanout(&self, topic: &str, frame: &[u8]) -> usize {
    let members: Vec<Pid> = {
      let state = self.lock();
      if state.stopped {
        return 0;
      }
      state
        .groups
        .members(topic)
        .into_iter()
        .filter(|p| p.node().as_str() != self.node())
        .collect()
    };
    if members.is_empty() {
      return 0;
    }
    let Ok(bytes) = postcard::to_allocvec(&HubCast::Frame(frame.to_vec())) else {
      return 0;
    };
    members
      .iter()
      .filter(|pid| self.mesh.remote_cast(pid, bytes.clone()).is_ok())
      .count()
  }

  fn nodes(&self, topic: &str) -> Vec<String> {
    let state = self.lock();
    self.others(&state, topic).into_iter().collect()
  }

  fn heartbeat(&self) {
    let found: BTreeMap<String, Pid> = self
      .mesh
      .global()
      .names()
      .into_iter()
      .filter_map(|(name, pid)| {
        let node = name.strip_prefix(&self.prefix)?.to_string();
        (node != self.node() && pid.node().as_str() == node).then_some((node, pid))
      })
      .collect();
    let alive: BTreeSet<String> = found.keys().cloned().collect();
    let (new, lost, snapshot, hubs) = {
      let mut state = self.lock();
      if state.stopped {
        return;
      }
      let lost: Vec<String> = state
        .hubs
        .iter()
        .filter(|(node, pid)| found.get(*node) != Some(*pid))
        .map(|(node, _)| node.clone())
        .collect();
      for node in &lost {
        state.hubs.remove(node);
        Self::forget_node(&mut state, node);
      }
      let new: Vec<(String, Pid)> = found
        .into_iter()
        .filter(|(node, _)| !state.hubs.contains_key(node))
        .collect();
      for (node, pid) in &new {
        state.lost.remove(node);
        state.hubs.insert(node.clone(), pid.clone());
      }
      (
        new,
        lost,
        Self::snapshot(&state),
        state.hubs.values().cloned().collect::<Vec<_>>(),
      )
    };
    for node in &lost {
      self.inbound.node_down(node);
    }
    // A node whose hub this one never found (it went down between its first frames and this
    // heartbeat) has no monitor here: its members go with its cluster-wide name.
    self.inbound.retain_nodes(&alive);
    let hello = HubCast::Hello {
      node: self.node().to_string(),
    };
    for (node, pid) in new {
      self.inbound.node_up(&node);
      self.cast(&pid, &hello);
      self.watch(node, pid);
    }
    for hub in hubs {
      self.send_groups(&hub, snapshot.clone());
    }
  }

  fn shutdown(&self) {
    {
      let mut state = self.lock();
      if state.stopped {
        return;
      }
      state.stopped = true;
    }
    let _ = self.mesh.global().unregister(&self.name);
    self.mesh.process().exit(&self.hub, ExitReason::Shutdown);
  }
}

/// A live session between this node's [`Mesh`] and one peer ([`attach_session`]): a node
/// [`SessionPump`]. Dropping it closes the session and reports the peer down.
pub struct SessionLink {
  pump: SessionPump,
  peer: String,
}

impl fmt::Debug for SessionLink {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SessionLink")
      .field("node", &self.node())
      .field("peer", &self.peer)
      .finish_non_exhaustive()
  }
}

impl SessionLink {
  fn new(pump: SessionPump) -> Self {
    let peer = pump.peer().name().to_owned();
    Self { pump, peer }
  }

  /// This side's node name.
  pub fn node(&self) -> &str {
    self.pump.node()
  }

  /// The peer's node name.
  pub fn peer(&self) -> &str {
    &self.peer
  }

  /// Whether the session still runs (the peer has not closed it).
  pub fn is_open(&self) -> bool {
    self.pump.is_open()
  }

  /// What the session's pump did with the envelopes the peer sent: delivered, refused by the
  /// mesh, or dropped as unverified.
  pub fn stats(&self) -> SessionPumpStats {
    self.pump.stats()
  }
}

/// Connect `nodes` pairwise over `net` (each listens at its node name) with the cookie
/// handshake and [`attach_session`] on both sides: a full in-memory mesh for tests and examples.
/// [`MemoryNetwork::partition`] between two names then cuts their session, as a lost network
/// would.
///
/// # Errors
///
/// [`ChannelError::Internal`] when a listener, a dial or a handshake fails.
///
/// # Panics
///
/// Outside a Tokio runtime.
///
/// [`MemoryNetwork::partition`]: id_effect_node::MemoryNetwork::partition
pub async fn connect_in_memory(
  net: &MemoryNetwork,
  nodes: &[(NodeId, Mesh)],
  cookie: &Cookie,
) -> Result<Vec<SessionLink>, ChannelError> {
  let internal = |e: &dyn fmt::Display| ChannelError::Internal(e.to_string());
  let mut listeners = Vec::new();
  for (id, _) in nodes {
    let listener = net
      .transport(id.name())
      .listen(id.name())
      .await
      .map_err(|e| internal(&e))?;
    listeners.push(listener);
  }
  let mut links = Vec::new();
  for (i, (a_id, a)) in nodes.iter().enumerate() {
    for (j, (b_id, b)) in nodes.iter().enumerate().skip(i + 1) {
      let dial = net
        .transport(a_id.name())
        .connect(b_id.name())
        .await
        .map_err(|e| internal(&e))?;
      let accepted = listeners[j].accept().await.map_err(|e| internal(&e))?;
      let (initiated, responded) = tokio::join!(
        Session::initiate(dial, a_id, cookie),
        Session::respond(accepted, b_id, cookie),
      );
      links.push(attach_session(a, initiated.map_err(|e| internal(&e))?));
      links.push(attach_session(b, responded.map_err(|e| internal(&e))?));
    }
  }
  Ok(links)
}

/// Run `mesh` over an authenticated `session` on the current Tokio runtime with a node
/// [`SessionPump`]: the peer becomes reachable ([`Mesh::add_peer_node`]), every envelope it
/// sends is verified against the session's peer (unverified ones are dropped and counted,
/// [`SessionLink::stats`]) and delivered, and when the session ends the peer is reported down
/// ([`Mesh::peer_down`]), which fires the monitors on its processes — the hubs' among them.
///
/// Envelopes are delivered through the node's verified path: the [`MeshRpc`] built over `mesh`
/// when there is one ([`MeshRpc::of`], looked up until found), so mesh RPC calls arriving over
/// this link are stamped with the verified peer and run; the mesh's own dispatch otherwise.
/// [`attach_session_rpc`] names the [`MeshRpc`] explicitly.
///
/// # Panics
///
/// Outside a Tokio runtime.
pub fn attach_session(mesh: &Mesh, session: Session) -> SessionLink {
  SessionLink::new(SessionPump::start(
    NodeIngress {
      mesh: mesh.clone(),
      rpc: OnceLock::new(),
    },
    session,
  ))
}

/// [`attach_session`] for a node with a [`MeshRpc`]: every envelope goes through
/// [`MeshRpc::ingress`], which stamps the RPC messages with the session's verified peer, so mesh
/// RPC and channels share one link.
///
/// # Panics
///
/// Outside a Tokio runtime.
pub fn attach_session_rpc(rpc: &MeshRpc, session: Session) -> SessionLink {
  SessionLink::new(SessionPump::start(rpc.clone(), session))
}

/// The ingress of a session attached to a bare [`Mesh`]: the node's [`MeshRpc`] once one exists.
struct NodeIngress {
  mesh: Mesh,
  rpc: OnceLock<MeshRpc>,
}

impl NodeIngress {
  fn rpc(&self) -> Option<&MeshRpc> {
    if let Some(rpc) = self.rpc.get() {
      return Some(rpc);
    }
    let found = MeshRpc::of(&self.mesh)?;
    Some(self.rpc.get_or_init(|| found))
  }
}

impl Ingress for NodeIngress {
  fn mesh(&self) -> &Mesh {
    &self.mesh
  }

  fn deliver(&self, origin: &NodeId, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    match self.rpc() {
      Some(rpc) => rpc.ingress(origin, envelope),
      None => self.mesh.handle_envelope_from(origin, envelope),
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use id_effect::{Exit, Runtime};
  use id_effect_node::Frame;
  use id_effect_process::{NodeName, ProcessNode};

  use super::*;

  struct Quiet;

  impl HubSink for Quiet {
    fn frame(&self, _: &[u8]) {}
    fn node_down(&self, _: &str) {}
    fn node_up(&self, _: &str) {}
    fn retain_nodes(&self, _: &BTreeSet<String>) {}
    fn peer_interest(&self, _: &str) {}
  }

  fn mesh(name: &str) -> (NodeId, Mesh) {
    let mut registry = BehaviorRegistry::new();
    register_hub(&mut registry);
    let node = NodeName::new(name);
    let id = NodeId::fresh(&node);
    (id.clone(), Mesh::new(id, ProcessNode::new(node), registry))
  }

  fn cookie() -> Cookie {
    Cookie::new("backplane-test-cookie-of-32-bytes!")
  }

  async fn until(done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
      assert!(tokio::time::Instant::now() < deadline, "timed out");
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn hubs_find_each_other_chunk_snapshots_and_stop() {
    let sink: Arc<dyn HubSink> = Arc::new(Quiet);
    let nodes = [mesh("a@bp"), mesh("b@bp")];
    let a = MeshBackplane::start(&nodes[0].1, "t", Inbound::new(Arc::downgrade(&sink))).unwrap();
    let b = MeshBackplane::start(&nodes[1].1, "t", Inbound::new(Arc::downgrade(&sink))).unwrap();
    let net = MemoryNetwork::new();
    let _links = connect_in_memory(&net, &nodes, &cookie()).await.unwrap();
    until(|| {
      a.heartbeat();
      b.heartbeat();
      a.hubs().contains_key("b@bp") && b.hubs().contains_key("a@bp")
    })
    .await;
    assert!(format!("{a:?}").contains("a@bp"));
    assert_eq!(a.hubs()["b@bp"], *b.hub());
    // The hub process answers calls (with nothing).
    let call = nodes[0].1.remote_call(
      b.hub(),
      postcard::to_allocvec(&()).unwrap(),
      Duration::from_secs(5),
    );
    assert!(matches!(
      Runtime::global().run(call).await,
      Exit::Success(_)
    ));
    // Many topics: the snapshot is sent in several casts, and b learns every one.
    let long = "x".repeat(200);
    for i in 0..300 {
      a.set_interest(&format!("{long}{i}"), true);
    }
    a.heartbeat();
    until(|| b.nodes(&format!("{long}299")) == ["a@bp"]).await;
    assert_eq!(a.fanout("nobody", b"frame"), 0);
    // Casts from a lost node are ignored until it is found again.
    b.lost("a@bp", a.hub());
    b.merge("a@bp", vec![]);
    b.hello("a@bp");
    assert!(b.nodes(&format!("{long}0")).is_empty());
    // Stopped: everything is a no-op, twice.
    a.shutdown();
    a.shutdown();
    a.set_interest("t", true);
    assert_eq!(a.fanout(&format!("{long}0"), b"frame"), 0);
    a.heartbeat();
    a.merge("b@bp", vec![]);
    a.hello("b@bp");
  }

  #[test]
  fn lost_nodes_are_remembered_within_a_bound() {
    let mut state = BpState {
      groups: ProcessGroups::new("n"),
      interest: BTreeSet::new(),
      hubs: BTreeMap::new(),
      lost: BTreeSet::new(),
      stopped: false,
    };
    for n in 0..=MAX_LOST {
      MeshBackplane::forget_node(&mut state, &format!("n{n}"));
    }
    assert_eq!(state.lost.len(), MAX_LOST);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn connecting_in_memory_reports_network_failures() {
    // Two nodes under one name: the second cannot listen.
    let (id, m) = mesh("dup@bp");
    let twice = [(id.clone(), m.clone()), (id, m)];
    let bound = connect_in_memory(&MemoryNetwork::new(), &twice, &cookie()).await;
    assert!(matches!(bound, Err(ChannelError::Internal(why)) if why.contains("bound")));
    // A partitioned pair cannot dial.
    let net = MemoryNetwork::new();
    net.partition("a@cut", "b@cut");
    let cut = connect_in_memory(&net, &[mesh("a@cut"), mesh("b@cut")], &cookie()).await;
    assert!(matches!(cut, Err(ChannelError::Internal(why)) if why.contains("partitioned")));
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_session_ends_on_an_undecodable_envelope() {
    let net = MemoryNetwork::new();
    let (a_id, _) = mesh("a@s");
    let (b_id, b) = mesh("b@s");
    let listener = net.transport("b@s").listen("b@s").await.unwrap();
    let dial = net.transport("a@s").connect("b@s").await.unwrap();
    let accepted = listener.accept().await.unwrap();
    let cookie = cookie();
    let (sa, sb) = tokio::join!(
      Session::initiate(dial, &a_id, &cookie),
      Session::respond(accepted, &b_id, &cookie),
    );
    let (sa, link) = (sa.unwrap(), attach_session(&b, sb.unwrap()));
    assert_eq!((link.node(), link.peer()), ("b@s", "a@s"));
    assert!(format!("{link:?}").contains("a@s"));
    sa.send(&Frame::Ping(1)).await.unwrap();
    assert!(link.is_open(), "a ping is not an envelope");
    sa.send(&Frame::Data(vec![0xff, 0xff, 0xff])).await.unwrap();
    until(|| !link.is_open()).await;
  }
}
