//! [`Mesh`] — process-layer distribution: remote spawn/send/call/link/monitor.
//!
//! Combines a local [`ProcessNode`], [`BehaviorRegistry`], and authenticated
//! [`Session`]s. Remote delivery is **at-most-once** (ADR 0009) unless a send opts in to
//! at-least-once ([`Mesh::send_reliable`], [`Mesh::cast_reliable`], or
//! [`DeliveryMode::AtLeastOnce`] for the whole mesh; see [`crate::delivery`]). On peer loss,
//! outstanding monitors fire [`ExitReason::NoConnection`].
//!
//! The effect sends ([`Mesh::cast`], [`Mesh::send`], the `*_reliable` sends and
//! [`Mesh::remote_call`]) carry the sending fiber's current span as the message's
//! [`TraceContext`] ([`Mesh::with_trace_propagator`]); the receiver opens the span that handles
//! the message under it (dist.remote#8). The plain-function sends ([`Mesh::remote_cast`],
//! [`Mesh::remote_info`]) run outside any fiber and carry none.
//!
//! Requests that wait for an answer ([`Mesh::remote_spawn`], [`Mesh::remote_call`]) are
//! Effects: `Send`, bounded by a timeout on the calling fiber's executor clock, and
//! interruptible (an interrupted request forgets its pending entry; the peer may still act on
//! it — at-most-once). An inbound call is answered by a daemon fiber on the local
//! [`ProcessNode`]'s runtime, never by a parked thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use id_effect::{Cause, Exit, Never, Span};
use id_effect::{Deferred, Effect, HNil, fail, sleep, succeed, suspend, sync};
use id_effect_process::{ExitReason, MonitorRef, MonitorSink, Pid, ProcessNode};

use crate::delivery::{Delivery, DeliveryConfig, DeliveryMode, DeliveryMonitor, Received};
use crate::envelope::{
  ExitReasonWire, PidWire, ProcessEnvelope, ReliableMessage, RemoteFault, TraceContext,
  TracePropagator, W3cTraceContext, decode_envelope, encode_envelope, traceparent_in,
};
use crate::global_registry::{self, GlobalNames, GlobalRegistry};
use crate::id::NodeId;
use crate::placement::{ClusterResourcePolicy, LoadReport, place};
use crate::registry::BehaviorRegistry;
use crate::session::Session;
use crate::wire::Frame;

/// How the mesh reaches a peer by node name.
pub type PeerSender = Arc<dyn Fn(ProcessEnvelope) -> Result<(), RemoteFault> + Send + Sync>;

/// In-memory peer fabric for deterministic multi-node tests.
#[derive(Clone, Default)]
pub struct MeshFabric {
  peers: Arc<Mutex<HashMap<String, Arc<Mesh>>>>,
}

impl MeshFabric {
  /// Empty fabric.
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a mesh under its node name.
  pub fn attach(&self, mesh: Arc<Mesh>) {
    let name = mesh.node_name().to_string();
    self.peers.lock().expect("fabric").insert(name, mesh);
  }

  /// Deliver an envelope to the named peer synchronously.
  ///
  /// The sender is unknown, so replies the envelope provokes are broadcast;
  /// meshes attached to the fabric route through `deliver_from` instead.
  pub fn deliver(&self, to_node: &str, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    self.peer(to_node)?.handle_envelope(envelope)
  }

  fn deliver_from(
    &self,
    from_node: &NodeId,
    to_node: &str,
    envelope: ProcessEnvelope,
  ) -> Result<(), RemoteFault> {
    self
      .peer(to_node)?
      .handle_envelope_from(from_node, envelope)
  }

  fn peer(&self, node: &str) -> Result<Arc<Mesh>, RemoteFault> {
    self
      .peers
      .lock()
      .expect("fabric")
      .get(node)
      .cloned()
      .ok_or(RemoteFault::NoProc)
  }
}

struct PendingCall {
  reply: Deferred<Vec<u8>, RemoteFault>,
}

struct PendingSpawn {
  reply: Deferred<Pid, RemoteFault>,
}

struct RemoteMonitor {
  sink: MonitorSink,
  target: Pid,
}

/// Which pending-request table an id lives in.
#[derive(Clone, Copy)]
enum PendingKind {
  Call,
  Spawn,
}

/// Key of a monitor a peer holds on one of our processes: the origin node
/// identity (name **and** incarnation, so a restarted node never collides
/// with its previous life) and the origin-side monitor reference carried on
/// the wire. `None` when the sender is unknown.
type InboundKey = (Option<NodeId>, u64);

struct MeshInner {
  node_id: NodeId,
  process: ProcessNode,
  registry: BehaviorRegistry,
  fabric: Option<MeshFabric>,
  /// Explicit senders for live sessions (node name → sender).
  peers: Mutex<HashMap<String, PeerSender>>,
  next_id: AtomicU64,
  pending_calls: Mutex<HashMap<u64, PendingCall>>,
  pending_spawns: Mutex<HashMap<u64, PendingSpawn>>,
  /// Local monitor refs allocated for remote targets.
  remote_monitors: Mutex<HashMap<u64, RemoteMonitor>>,
  /// Monitors peers hold on our local processes: (origin, origin ref) →
  /// (target, local ref). Lets `Demonitor` find the local registration.
  inbound_monitors: Mutex<HashMap<InboundKey, (Pid, MonitorRef)>>,
  /// Per-node load telemetry for placement (node name -> (cpu_pct, mem_pct)).
  /// Populated by a telemetry/gossip loop (or directly in tests); read by
  /// [`Mesh::load_reports`] to feed [`crate::placement::place`].
  loads: Mutex<HashMap<String, (f32, f32)>>,
  /// This node's replica of the cluster-wide name table ([`Mesh::global`]).
  global_names: Mutex<GlobalNames>,
  /// At-least-once state: config, retransmit buffers, dedup windows ([`crate::delivery`]).
  delivery: Delivery,
  /// How spans cross the wire ([`Mesh::with_trace_propagator`]).
  trace: Mutex<Arc<dyn TracePropagator>>,
}

/// Distribution façade for one node.
#[derive(Clone)]
pub struct Mesh {
  inner: Arc<MeshInner>,
}

impl Mesh {
  /// Build a mesh around an existing process node + registry.
  pub fn new(node_id: NodeId, process: ProcessNode, registry: BehaviorRegistry) -> Self {
    Self {
      inner: Arc::new(MeshInner {
        node_id,
        process,
        registry,
        fabric: None,
        peers: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        pending_calls: Mutex::new(HashMap::new()),
        pending_spawns: Mutex::new(HashMap::new()),
        remote_monitors: Mutex::new(HashMap::new()),
        inbound_monitors: Mutex::new(HashMap::new()),
        loads: Mutex::new(HashMap::new()),
        global_names: Mutex::new(GlobalNames::default()),
        delivery: Delivery::default(),
        trace: Mutex::new(Arc::new(W3cTraceContext::new())),
      }),
    }
  }

  /// Use `config` for remote casts and sends (default: at-most-once, [`DeliveryConfig`]).
  pub fn with_delivery(self, config: DeliveryConfig) -> Self {
    self.inner.delivery.set_config(config);
    self
  }

  /// The delivery settings in force.
  pub fn delivery(&self) -> DeliveryConfig {
    self.inner.delivery.config()
  }

  /// Report every at-least-once message this node gives up ([`crate::delivery::DeliveryFailed`])
  /// to `monitor`.
  pub fn monitor_delivery(&self, monitor: DeliveryMonitor) {
    self.inner.delivery.add_monitor(monitor);
  }

  /// Carry spans across the wire with `propagator` (default [`W3cTraceContext`], which opens no
  /// receiving spans).
  pub fn with_trace_propagator(self, propagator: Arc<dyn TracePropagator>) -> Self {
    *self.inner.trace.lock().expect("trace") = propagator;
    self
  }

  fn propagator(&self) -> Arc<dyn TracePropagator> {
    Arc::clone(&self.inner.trace.lock().expect("trace"))
  }

  /// Attach to an in-memory fabric (tests).
  pub fn with_fabric(mut self, fabric: MeshFabric) -> Arc<Self> {
    let inner = Arc::get_mut(&mut self.inner).expect("unique");
    inner.fabric = Some(fabric.clone());
    let mesh = Arc::new(self);
    fabric.attach(Arc::clone(&mesh));
    mesh
  }

  /// Logical node name (`name@host`).
  pub fn node_name(&self) -> &str {
    self.inner.node_id.name()
  }

  /// Local process node.
  pub fn process(&self) -> &ProcessNode {
    &self.inner.process
  }

  /// Behavior registry.
  pub fn registry(&self) -> &BehaviorRegistry {
    &self.inner.registry
  }

  /// Cluster-wide name registry ([`GlobalRegistry`]) backed by this node's replica.
  pub fn global(&self) -> GlobalRegistry {
    GlobalRegistry::new(self.clone())
  }

  pub(crate) fn node_id(&self) -> &NodeId {
    &self.inner.node_id
  }

  pub(crate) fn global_names(&self) -> &Mutex<GlobalNames> {
    &self.inner.global_names
  }

  /// Register a peer sender (production session path). The peer is sent the global names
  /// this node owns, if any ([`GlobalRegistry::sync_with`]), and every at-least-once message it
  /// has not acknowledged yet.
  pub fn add_peer(&self, node_name: impl Into<String>, sender: PeerSender) {
    self.connect_peer(node_name.into(), None, sender);
  }

  /// [`Mesh::add_peer`] for an authenticated peer identity ([`Session::peer`]): if `node` is a
  /// new incarnation of the peer, the at-least-once messages its previous life did not
  /// acknowledge fail with [`DeliveryFailure::PeerRestarted`](crate::DeliveryFailure) instead of
  /// being sent again.
  pub fn add_peer_node(&self, node: &NodeId, sender: PeerSender) {
    self.connect_peer(node.name().to_string(), Some(node.incarnation()), sender);
  }

  fn connect_peer(&self, node_name: String, incarnation: Option<u128>, sender: PeerSender) {
    self
      .inner
      .peers
      .lock()
      .expect("peers")
      .insert(node_name.clone(), sender);
    global_registry::peer_up(self, &node_name);
    let reconnected = self.inner.delivery.on_connect(&node_name, incarnation);
    self.inner.delivery.notify(reconnected.failed);
    for frame in reconnected.frames {
      let _ = self.send_to(&node_name, frame);
    }
    if reconnected.start_driver {
      self.start_retransmit(&node_name);
    }
  }

  /// Notify that a peer node is down: fire our monitors on its processes
  /// with `noconnection`, and drop the monitors it held on ours (every
  /// incarnation of that node name).
  pub fn peer_down(&self, peer_name: &str) {
    // Collect under the lock, invoke after releasing it: a sink may
    // re-enter the mesh (e.g. `remote_monitor`).
    let doomed: Vec<(u64, RemoteMonitor)> = {
      let mut monitors = self.inner.remote_monitors.lock().expect("monitors");
      let keys: Vec<u64> = monitors
        .iter()
        .filter(|(_, m)| m.target.node().as_str() == peer_name)
        .map(|(k, _)| *k)
        .collect();
      keys
        .into_iter()
        .filter_map(|k| monitors.remove(&k).map(|m| (k, m)))
        .collect()
    };
    for (key, m) in doomed {
      (m.sink)(
        MonitorRef::from_inner(key),
        m.target,
        ExitReason::NoConnection,
      );
    }
    let held: Vec<(Pid, MonitorRef)> = {
      let mut inbound = self.inner.inbound_monitors.lock().expect("inbound");
      let keys: Vec<InboundKey> = inbound
        .keys()
        .filter(|(origin, _)| origin.as_ref().is_some_and(|o| o.name() == peer_name))
        .cloned()
        .collect();
      keys
        .into_iter()
        .filter_map(|k| inbound.remove(&k))
        .collect()
    };
    for (pid, local) in held {
      self.inner.process.demonitor(&pid, local);
    }
    global_registry::node_down(self, peer_name);
    self.inner.peers.lock().expect("peers").remove(peer_name);
  }

  fn alloc_id(&self) -> u64 {
    self.inner.next_id.fetch_add(1, Ordering::Relaxed)
  }

  fn send_to(&self, node_name: &str, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    if let Some(sender) = self.inner.peers.lock().expect("peers").get(node_name) {
      return sender(envelope);
    }
    if let Some(fabric) = &self.inner.fabric {
      return fabric.deliver_from(&self.inner.node_id, node_name, envelope);
    }
    Err(RemoteFault::NoProc)
  }

  pub(crate) fn send_envelope(
    &self,
    node_name: &str,
    envelope: ProcessEnvelope,
  ) -> Result<(), RemoteFault> {
    self.send_to(node_name, envelope)
  }

  /// Best-effort send to every known peer (session senders and fabric members), naming this
  /// node as the sender.
  pub(crate) fn broadcast_to_peers(&self, envelope: ProcessEnvelope) {
    let mut names: Vec<String> = self
      .inner
      .peers
      .lock()
      .expect("peers")
      .keys()
      .cloned()
      .collect();
    if let Some(fabric) = &self.inner.fabric {
      names.extend(fabric.peers.lock().expect("fabric").keys().cloned());
    }
    names.sort();
    names.dedup();
    for name in names.iter().filter(|n| n.as_str() != self.node_name()) {
      let _ = self.send_to(name, envelope.clone());
    }
  }

  /// Local spawn through the registry (installs wire route).
  pub fn spawn_local(
    &self,
    behavior: &str,
    args: &[u8],
    link_to: Option<&Pid>,
  ) -> Result<Pid, RemoteFault> {
    self
      .inner
      .registry
      .spawn(&self.inner.process, behavior, args, link_to)
  }

  /// Remote spawn on `node_name` (a local spawn when it names this node).
  ///
  /// The effect sends the request when it runs and waits for the reply up to `timeout` on the
  /// calling fiber's executor clock ([`RemoteFault::Timeout`] after that). It is interruptible;
  /// the pending entry is removed on every exit path.
  pub fn remote_spawn<R: Send + Sync + 'static>(
    &self,
    node_name: &str,
    behavior: &str,
    args: Vec<u8>,
    link_to: Option<&Pid>,
    timeout: Duration,
  ) -> Effect<Pid, RemoteFault, R> {
    let mesh = self.clone();
    let node_name = node_name.to_string();
    let behavior = behavior.to_string();
    let link_to = link_to.cloned();
    suspend(move || {
      if node_name == mesh.node_name() {
        return match mesh.spawn_local(&behavior, &args, link_to.as_ref()) {
          Ok(pid) => succeed(pid),
          Err(fault) => fail(fault),
        };
      }
      let spawn_id = mesh.alloc_id();
      let m = mesh.clone();
      mesh.pending_request(PendingKind::Spawn, spawn_id, move || {
        let reply: Deferred<Pid, RemoteFault> = Deferred::new();
        m.inner.pending_spawns.lock().expect("spawns").insert(
          spawn_id,
          PendingSpawn {
            reply: reply.clone(),
          },
        );
        let sent = m.send_to(
          &node_name,
          ProcessEnvelope::Spawn {
            spawn_id,
            behavior,
            args,
            link_to: link_to.as_ref().map(PidWire::from),
          },
        );
        await_reply(sent, reply, timeout)
      })
    })
  }

  /// Record a node's latest load telemetry for placement.
  ///
  /// A telemetry/gossip loop calls this for each reachable node (including
  /// self); tests can call it directly. Utilizations are fractions in
  /// `[0.0, 1.0]`. The latest report per node wins.
  pub fn report_load(&self, node: impl Into<String>, cpu_pct: f32, mem_pct: f32) {
    self
      .inner
      .loads
      .lock()
      .expect("loads")
      .insert(node.into(), (cpu_pct, mem_pct));
  }

  /// Snapshot the recorded per-node load telemetry as [`LoadReport`]s.
  ///
  /// Order is unspecified; [`place`] is order-independent (deterministic ties by
  /// node name), so callers need not sort.
  pub fn load_reports(&self) -> Vec<LoadReport> {
    self
      .inner
      .loads
      .lock()
      .expect("loads")
      .iter()
      .map(|(node, (cpu, mem))| LoadReport::new(node.clone(), *cpu, *mem))
      .collect()
  }

  /// Spawn a behavior on the node chosen by [`place`] from `reports`/`policy`.
  ///
  /// This is the placement-driven counterpart to [`Mesh::remote_spawn`]: instead
  /// of an explicit node name, the caller supplies load telemetry and a cluster
  /// policy, and the mesh resolves the target. Returns [`RemoteFault::NoProc`]
  /// when no reported node satisfies the policy (the caller can then fall back to
  /// an explicit node).
  pub fn remote_spawn_placed<R: Send + Sync + 'static>(
    &self,
    reports: &[LoadReport],
    policy: &ClusterResourcePolicy,
    behavior: &str,
    args: Vec<u8>,
    link_to: Option<&Pid>,
    timeout: Duration,
  ) -> Effect<Pid, RemoteFault, R> {
    match place(reports, policy) {
      Some(target) => self.remote_spawn(&target, behavior, args, link_to, timeout),
      None => fail(RemoteFault::NoProc),
    }
  }

  /// Remote cast (or local if same node), with the mesh's [`DeliveryMode`]. A plain function:
  /// it carries no trace context ([`Mesh::cast`] does).
  pub fn remote_cast(&self, to: &Pid, msg: Vec<u8>) -> Result<(), RemoteFault> {
    self.route(
      ReliableMessage::Cast {
        to: PidWire::from(to),
        msg,
        trace: TraceContext::new(),
      },
      self.at_least_once(),
    )
  }

  /// Remote info send, with the mesh's [`DeliveryMode`]. A plain function: it carries no trace
  /// context ([`Mesh::send`] does).
  pub fn remote_info(&self, to: &Pid, msg: Vec<u8>) -> Result<(), RemoteFault> {
    self.route(
      ReliableMessage::Info {
        to: PidWire::from(to),
        msg,
        trace: TraceContext::new(),
      },
      self.at_least_once(),
    )
  }

  /// Cast `msg` to `to` (local or remote) with the mesh's [`DeliveryMode`], carrying the
  /// running fiber's trace context.
  pub fn cast<R: Send + Sync + 'static>(
    &self,
    to: &Pid,
    msg: Vec<u8>,
  ) -> Effect<(), RemoteFault, R> {
    self.traced_send(to, msg, false, None)
  }

  /// Send `msg` to `to`'s `handle_info` (local or remote) with the mesh's [`DeliveryMode`],
  /// carrying the running fiber's trace context.
  pub fn send<R: Send + Sync + 'static>(
    &self,
    to: &Pid,
    msg: Vec<u8>,
  ) -> Effect<(), RemoteFault, R> {
    self.traced_send(to, msg, true, None)
  }

  /// [`Mesh::cast`] at-least-once whatever the mesh's mode ([`crate::delivery`]). Succeeds once
  /// the message is buffered for retransmission; a message given up later is reported to the
  /// [`Mesh::monitor_delivery`] monitors.
  pub fn cast_reliable<R: Send + Sync + 'static>(
    &self,
    to: &Pid,
    msg: Vec<u8>,
  ) -> Effect<(), RemoteFault, R> {
    self.traced_send(to, msg, false, Some(true))
  }

  /// [`Mesh::send`] at-least-once whatever the mesh's mode ([`crate::delivery`]). Succeeds once
  /// the message is buffered for retransmission; a message given up later is reported to the
  /// [`Mesh::monitor_delivery`] monitors.
  pub fn send_reliable<R: Send + Sync + 'static>(
    &self,
    to: &Pid,
    msg: Vec<u8>,
  ) -> Effect<(), RemoteFault, R> {
    self.traced_send(to, msg, true, Some(true))
  }

  fn at_least_once(&self) -> bool {
    self.inner.delivery.config().mode == DeliveryMode::AtLeastOnce
  }

  /// A cast (`info == false`) or info carrying the fiber's trace context; `reliable` overrides
  /// the mesh's mode.
  fn traced_send<R: Send + Sync + 'static>(
    &self,
    to: &Pid,
    msg: Vec<u8>,
    info: bool,
    reliable: Option<bool>,
  ) -> Effect<(), RemoteFault, R> {
    let mesh = self.clone();
    let to = PidWire::from(to);
    self.outgoing_trace().flat_map(move |trace| {
      let message = if info {
        ReliableMessage::Info { to, msg, trace }
      } else {
        ReliableMessage::Cast { to, msg, trace }
      };
      let reliable = reliable.unwrap_or_else(|| mesh.at_least_once());
      match mesh.route(message, reliable) {
        Ok(()) => succeed(()),
        Err(fault) => fail(fault),
      }
    })
  }

  /// The running fiber's current span as a [`TraceContext`] (empty outside any span).
  fn outgoing_trace<E: Send + 'static, R: Send + Sync + 'static>(
    &self,
  ) -> Effect<TraceContext, E, R> {
    let propagator = self.propagator();
    Span::current().map(move |span| {
      let mut carrier = TraceContext::new();
      if let Some(span) = span {
        propagator.inject(&span, &mut carrier);
      }
      carrier
    })
  }

  /// Deliver locally, or send at-most-once, or buffer and send at-least-once.
  fn route(&self, message: ReliableMessage, reliable: bool) -> Result<(), RemoteFault> {
    let node = message.to().node.clone();
    if node == self.node_name() {
      return match message {
        ReliableMessage::Cast { to, msg, .. } => {
          self.inner.registry.deliver_cast(&to.to_pid(), &msg)
        }
        ReliableMessage::Info { to, msg, .. } => {
          self.inner.registry.deliver_info(&to.to_pid(), &msg)
        }
      };
    }
    if !reliable {
      return self.send_to(&node, message.into_envelope());
    }
    let enqueued = self.inner.delivery.enqueue(&node, message)?;
    self.inner.delivery.notify(enqueued.evicted);
    // A failed transmission stays buffered: the next reconnect or retransmission round sends it.
    let _ = self.send_to(&node, enqueued.frame);
    if enqueued.start_driver {
      self.start_retransmit(&node);
    }
    Ok(())
  }

  /// Start `peer`'s retransmission driver: a daemon fiber on this node's runtime that sends the
  /// unacknowledged messages again with backoff and stops when none are left (or the mesh is
  /// dropped).
  fn start_retransmit(&self, peer: &str) {
    let delay = self.inner.delivery.config().backoff(0);
    let driver = retransmit_loop(Arc::downgrade(&self.inner), peer.to_string(), delay);
    drop(self.inner.process.runtime().fork(driver));
  }

  /// Unacknowledged at-least-once messages buffered for `peer`.
  pub fn unacked(&self, peer: &str) -> usize {
    self.inner.delivery.unacked(peer)
  }

  /// Remote call with timeout (a local call when `to` lives on this node).
  ///
  /// Like [`Mesh::remote_spawn`]: sent when the effect runs, bounded by `timeout` on the
  /// calling fiber's executor clock, interruptible, and leaves no pending entry behind. The
  /// request carries the calling fiber's trace context.
  pub fn remote_call<R: Send + Sync + 'static>(
    &self,
    to: &Pid,
    msg: Vec<u8>,
    timeout: Duration,
  ) -> Effect<Vec<u8>, RemoteFault, R> {
    let mesh = self.clone();
    let to = to.clone();
    suspend(move || {
      if to.node().as_str() == mesh.node_name() {
        return match mesh.inner.registry.deliver_call::<R>(&to, &msg) {
          Ok(reply) => reply.timeout_fail(timeout, || RemoteFault::Timeout),
          Err(fault) => fail(fault),
        };
      }
      let call_id = mesh.alloc_id();
      let m = mesh.clone();
      mesh.outgoing_trace().flat_map(move |trace| {
        let p = m.clone();
        m.pending_request(PendingKind::Call, call_id, move || {
          let reply: Deferred<Vec<u8>, RemoteFault> = Deferred::new();
          p.inner.pending_calls.lock().expect("calls").insert(
            call_id,
            PendingCall {
              reply: reply.clone(),
            },
          );
          let sent = p.send_to(
            to.node().as_str(),
            ProcessEnvelope::Call {
              to: PidWire::from(&to),
              call_id,
              msg,
              trace,
            },
          );
          await_reply(sent, reply, timeout)
        })
      })
    })
  }

  /// Monitor a (possibly remote) pid; sink fires exactly once.
  pub fn remote_monitor(&self, target: &Pid, sink: MonitorSink) -> MonitorRef {
    if target.node().as_str() == self.node_name() {
      return self.inner.process.monitor_with_sink(target, sink);
    }
    let monitor = MonitorRef::fresh();
    let key = monitor.into_inner();
    self.inner.remote_monitors.lock().expect("monitors").insert(
      key,
      RemoteMonitor {
        sink,
        target: target.clone(),
      },
    );
    let sent = self.send_to(
      target.node().as_str(),
      ProcessEnvelope::Monitor {
        monitor: key,
        target: PidWire::from(target),
      },
    );
    if sent.is_err() {
      // Unreachable node: fire immediately, like BEAM's `noconnection`.
      let doomed = self
        .inner
        .remote_monitors
        .lock()
        .expect("monitors")
        .remove(&key);
      if let Some(m) = doomed {
        (m.sink)(
          MonitorRef::from_inner(key),
          m.target,
          ExitReason::NoConnection,
        );
      }
    }
    MonitorRef::from_inner(key)
  }

  /// Cancel a monitor set up with [`Mesh::remote_monitor`]. After this
  /// returns the sink never fires. Idempotent; unknown refs are ignored.
  pub fn remote_demonitor(&self, target: &Pid, monitor: MonitorRef) {
    if target.node().as_str() == self.node_name() {
      self.inner.process.demonitor(target, monitor);
      return;
    }
    let key = monitor.into_inner();
    let removed = self
      .inner
      .remote_monitors
      .lock()
      .expect("monitors")
      .remove(&key);
    if removed.is_some() {
      // Best effort: if the peer is unreachable its registration dies with
      // the connection anyway.
      let _ = self.send_to(
        target.node().as_str(),
        ProcessEnvelope::Demonitor {
          monitor: key,
          target: PidWire::from(target),
        },
      );
    }
  }

  /// Handle an inbound envelope whose sender is unknown.
  ///
  /// Replies it provokes (`Reply`, `SpawnReply`, `Down`) are broadcast to
  /// every peer. Prefer [`Mesh::handle_envelope_from`] whenever the sending
  /// node is known, so replies reach only the caller.
  pub fn handle_envelope(&self, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    self.dispatch(None, envelope)
  }

  /// Handle an inbound envelope sent by the node `origin` (as authenticated
  /// by the session handshake). Replies are routed back to `origin` only,
  /// and state kept on its behalf is keyed by its full identity, so a
  /// restarted node (same name, new incarnation) never collides with its
  /// previous life.
  pub fn handle_envelope_from(
    &self,
    origin: &NodeId,
    envelope: ProcessEnvelope,
  ) -> Result<(), RemoteFault> {
    self.dispatch(Some(origin), envelope)
  }

  fn dispatch(
    &self,
    origin: Option<&NodeId>,
    envelope: ProcessEnvelope,
  ) -> Result<(), RemoteFault> {
    let origin_name = origin.map(NodeId::name);
    match envelope {
      ProcessEnvelope::Cast { to, msg, trace } => {
        let pid = to.to_pid();
        let delivered = self.inner.registry.deliver_cast(&pid, &msg);
        self.record_receipt("mesh.cast", origin_name, &trace);
        delivered
      }
      ProcessEnvelope::Info { to, msg, trace } => {
        let pid = to.to_pid();
        let delivered = self.inner.registry.deliver_info(&pid, &msg);
        self.record_receipt("mesh.info", origin_name, &trace);
        delivered
      }
      ProcessEnvelope::Call {
        to,
        call_id,
        msg,
        trace,
      } => {
        let pid = to.to_pid();
        let wait = match self.inner.registry.deliver_call::<HNil>(&pid, &msg) {
          Ok(wait) => wait,
          Err(fault) => {
            self.reply(
              origin_name,
              ProcessEnvelope::Reply {
                call_id,
                result: Err(fault),
              },
            );
            return Ok(());
          }
        };
        // The answer is relayed by a daemon fiber on this node's runtime. The caller bounds the
        // wait with its own timeout; this side has none (a lost reply is a caller timeout).
        let mesh = self.clone();
        let origin = origin_name.map(str::to_string);
        let relay: Effect<(), Never> = wait.exit().flat_map(move |exit| {
          sync(move || {
            let result = match exit {
              Exit::Success(bytes) => Ok(bytes),
              Exit::Failure(Cause::Fail(f)) => Err(f),
              Exit::Failure(_) => Err(RemoteFault::Down(ExitReasonWire::Interrupted)),
            };
            mesh.reply(
              origin.as_deref(),
              ProcessEnvelope::Reply { call_id, result },
            );
          })
        });
        // The caller's span is the parent of the span that covers handling the call.
        let relay = self
          .receiving_span(relay, "mesh.call", "server", origin_name, &trace)
          .unwrap_or_else(|untraced| untraced);
        drop(self.inner.process.runtime().fork(relay));
        Ok(())
      }
      ProcessEnvelope::Reply { call_id, result } => {
        if let Some(pending) = self
          .inner
          .pending_calls
          .lock()
          .expect("calls")
          .remove(&call_id)
        {
          match result {
            Ok(bytes) => {
              let _ = pending.reply.try_succeed(bytes);
            }
            Err(f) => {
              let _ = pending.reply.try_fail_cause(Cause::fail(f));
            }
          }
        }
        Ok(())
      }
      ProcessEnvelope::Spawn {
        spawn_id,
        behavior,
        args,
        link_to,
      } => {
        let link = link_to.as_ref().map(|p| p.to_pid());
        let result = self
          .inner
          .registry
          .spawn(&self.inner.process, &behavior, &args, link.as_ref())
          .map(|pid| PidWire::from(&pid));
        self.reply(
          origin_name,
          ProcessEnvelope::SpawnReply { spawn_id, result },
        );
        Ok(())
      }
      ProcessEnvelope::SpawnReply { spawn_id, result } => {
        if let Some(pending) = self
          .inner
          .pending_spawns
          .lock()
          .expect("spawns")
          .remove(&spawn_id)
        {
          match result {
            Ok(pid) => {
              let _ = pending.reply.try_succeed(pid.to_pid());
            }
            Err(f) => {
              let _ = pending.reply.try_fail_cause(Cause::fail(f));
            }
          }
        }
        Ok(())
      }
      ProcessEnvelope::Monitor { monitor, target } => {
        let pid = target.to_pid();
        let mesh = self.clone();
        let key: InboundKey = (origin.cloned(), monitor);
        let fired = Arc::new(AtomicBool::new(false));
        let sink: MonitorSink = {
          let (key, fired) = (key.clone(), Arc::clone(&fired));
          Box::new(move |_m, target_pid, reason| {
            fired.store(true, Ordering::SeqCst);
            mesh
              .inner
              .inbound_monitors
              .lock()
              .expect("inbound")
              .remove(&key);
            mesh.reply(
              key.0.as_ref().map(NodeId::name),
              ProcessEnvelope::Down {
                monitor,
                target: PidWire::from(&target_pid),
                reason: ExitReasonWire::from(&reason),
              },
            );
          })
        };
        let local = self.inner.process.monitor_with_sink(&pid, sink);
        let mut inbound = self.inner.inbound_monitors.lock().expect("inbound");
        // A sink that already fired (dead target) must not leave an entry.
        if !fired.load(Ordering::SeqCst) {
          inbound.insert(key, (pid, local));
        }
        Ok(())
      }
      ProcessEnvelope::Demonitor { monitor, .. } => {
        let key: InboundKey = (origin.cloned(), monitor);
        let entry = self
          .inner
          .inbound_monitors
          .lock()
          .expect("inbound")
          .remove(&key);
        if let Some((pid, local)) = entry {
          self.inner.process.demonitor(&pid, local);
        }
        Ok(())
      }
      ProcessEnvelope::Down {
        monitor,
        target,
        reason,
      } => {
        // Release the lock before invoking the sink: it may re-enter.
        let fired = self
          .inner
          .remote_monitors
          .lock()
          .expect("monitors")
          .remove(&monitor);
        if let Some(m) = fired {
          (m.sink)(
            MonitorRef::from_inner(monitor),
            target.to_pid(),
            ExitReason::from(&reason),
          );
        }
        Ok(())
      }
      ProcessEnvelope::Link { from, to } => {
        // Cross-node link v1: monitor the local target and forward Down as
        // ExitSignal to the remote `from` (fate-share one direction; the
        // peer installs the symmetric half).
        if to.to_pid().node().as_str() == self.node_name() {
          let mesh = self.clone();
          let from_w = from.clone();
          let to_pid = to.to_pid();
          let sink: MonitorSink = Box::new(move |_m, target_pid, reason| {
            let _ = mesh.send_to(
              from_w.node.as_str(),
              ProcessEnvelope::ExitSignal {
                from: PidWire::from(&target_pid),
                to: from_w.clone(),
                reason: ExitReasonWire::from(&reason),
              },
            );
          });
          let _ = self.inner.process.monitor_with_sink(&to_pid, sink);
        }
        Ok(())
      }
      ProcessEnvelope::Unlink { .. } => Ok(()),
      ProcessEnvelope::ExitSignal { from, to, reason } => {
        let target = to.to_pid();
        if target.node().as_str() == self.node_name() {
          self.inner.process.exit(&target, ExitReason::from(&reason));
          let _ = from;
        }
        Ok(())
      }
      ProcessEnvelope::GlobalRegister { claim } => {
        global_registry::on_register(self, origin, claim);
        Ok(())
      }
      ProcessEnvelope::GlobalUnregister { name, pid } => {
        global_registry::on_unregister(self, origin, &name, &pid);
        Ok(())
      }
      ProcessEnvelope::GlobalSync { claims } => {
        global_registry::on_sync(self, origin, claims);
        Ok(())
      }
      ProcessEnvelope::Reliable {
        seq,
        floor,
        message,
      } => {
        // Without an authenticated sender there is no window to dedup in: at-most-once.
        let Some(from) = origin else {
          return self.dispatch(None, message.into_envelope());
        };
        let (received, upto) = self.inner.delivery.receive(from, seq, floor);
        let delivered = match received {
          Received::Fresh => self.dispatch(origin, message.into_envelope()),
          Received::Duplicate => Ok(()),
          Received::OutOfWindow => return Ok(()),
        };
        // Acknowledged after delivery, whatever its outcome: retransmitting cannot help.
        self.reply(origin_name, ProcessEnvelope::Ack { upto, seq });
        delivered
      }
      ProcessEnvelope::Ack { upto, seq } => {
        if let Some(from) = origin {
          let failed = self.inner.delivery.on_ack(from, upto, seq);
          self.inner.delivery.notify(failed);
        }
        Ok(())
      }
    }
  }

  /// `Ok(eff)` in the receiving span `name` of a message that carried `trace`, under the
  /// propagator's hook (so the sender's span is its parent); `Err(eff)`, untouched, when the
  /// message carried no context or the propagator opens no span.
  fn receiving_span(
    &self,
    eff: Effect<(), Never>,
    name: &'static str,
    kind: &'static str,
    origin: Option<&str>,
    trace: &TraceContext,
  ) -> Result<Effect<(), Never>, Effect<(), Never>> {
    if trace.is_empty() {
      return Err(eff);
    }
    let Some(hook) = self.propagator().receiver_hook(trace) else {
      return Err(eff);
    };
    let mut attributes = vec![
      ("otel.kind".to_string(), kind.to_string()),
      (
        "mesh.from".to_string(),
        origin.unwrap_or("unknown").to_string(),
      ),
    ];
    if let Some(parent) = traceparent_in(trace) {
      attributes.push(("mesh.parent".to_string(), parent.to_string()));
    }
    Ok(eff.with_span_attrs(name, attributes).with_span_hook(hook))
  }

  /// Record the receipt of a cast or info that carried `trace` as a span under the sender's.
  /// The message is already in the mailbox (delivery stays synchronous and ordered); the span
  /// runs on a daemon fiber.
  fn record_receipt(&self, name: &'static str, origin: Option<&str>, trace: &TraceContext) {
    if let Ok(span) = self.receiving_span(succeed(()), name, "consumer", origin, trace) {
      drop(self.inner.process.runtime().fork(span));
    }
  }

  /// Route a reply to the requesting node, or broadcast when it is unknown.
  /// Best effort: a lost reply surfaces as a timeout at the caller.
  fn reply(&self, origin: Option<&str>, envelope: ProcessEnvelope) {
    match origin {
      Some(node) => {
        let _ = self.send_to(node, envelope);
      }
      None => self.broadcast_except_self(envelope),
    }
  }

  fn broadcast_except_self(&self, envelope: ProcessEnvelope) {
    if let Some(fabric) = &self.inner.fabric {
      let names: Vec<String> = fabric
        .peers
        .lock()
        .expect("fabric")
        .keys()
        .filter(|k| k.as_str() != self.node_name())
        .cloned()
        .collect();
      for name in names {
        let _ = fabric.deliver(&name, envelope.clone());
      }
    }
    let peers: Vec<_> = self
      .inner
      .peers
      .lock()
      .expect("peers")
      .iter()
      .map(|(n, s)| (n.clone(), Arc::clone(s)))
      .collect();
    for (_n, sender) in peers {
      let _ = sender(envelope.clone());
    }
  }

  /// Forget a pending request (reply, send failure, timeout or interrupt). After a reply the
  /// entry is already gone and this is a no-op.
  fn forget_pending(&self, kind: PendingKind, id: u64) {
    match kind {
      PendingKind::Call => {
        self.inner.pending_calls.lock().expect("calls").remove(&id);
      }
      PendingKind::Spawn => {
        self
          .inner
          .pending_spawns
          .lock()
          .expect("spawns")
          .remove(&id);
      }
    }
  }

  /// Run `body` (register request `id`, send it, wait) with the pending entry forgotten on
  /// every exit path. The finalizer is in place before `body` registers anything, so an
  /// interrupt at any point leaves no entry behind.
  fn pending_request<T, R>(
    &self,
    kind: PendingKind,
    id: u64,
    body: impl FnOnce() -> Effect<T, RemoteFault, R> + Send + 'static,
  ) -> Effect<T, RemoteFault, R>
  where
    T: Send + 'static,
    R: Send + Sync + 'static,
  {
    let mesh = self.clone();
    suspend(body).ensuring(sync(move || mesh.forget_pending(kind, id)))
  }

  /// Pump one inbound data frame from a session into the mesh.
  pub async fn handle_session_frame(&self, session: &Session) -> Result<(), RemoteFault> {
    let frame = session
      .recv()
      .await
      .map_err(|e| RemoteFault::Decode(e.to_string()))?;
    match frame {
      Frame::Data(bytes) => {
        let envelope = decode_envelope(&bytes).map_err(|e| RemoteFault::Decode(e.to_string()))?;
        self.handle_envelope_from(session.peer(), envelope)
      }
      Frame::Close(_) => Err(RemoteFault::NoProc),
      _ => Ok(()),
    }
  }

  /// Encode + send an envelope over a live session.
  pub async fn session_send(
    session: &Session,
    envelope: &ProcessEnvelope,
  ) -> Result<(), RemoteFault> {
    let bytes = encode_envelope(envelope).map_err(|e| RemoteFault::Decode(e.to_string()))?;
    session
      .send(&Frame::Data(bytes))
      .await
      .map_err(|e| RemoteFault::Decode(e.to_string()))
  }
}

/// `peer`'s retransmission driver: after `delay`, send every unacknowledged message again and
/// go round with the backed-off delay, until nothing is unacknowledged or the mesh is gone.
fn retransmit_loop(mesh: Weak<MeshInner>, peer: String, delay: Duration) -> Effect<(), Never> {
  sleep(delay).flat_map(move |()| {
    let Some(inner) = mesh.upgrade() else {
      return succeed(());
    };
    let live = Mesh { inner };
    let Some((frames, next)) = live.inner.delivery.tick(&peer) else {
      return succeed(());
    };
    for frame in frames {
      let _ = live.send_to(&peer, frame);
    }
    drop(live);
    retransmit_loop(mesh, peer, next)
  })
}

/// Wait for the reply to a request handed to the peer (`sent`), up to `timeout`.
fn await_reply<T, R>(
  sent: Result<(), RemoteFault>,
  reply: Deferred<T, RemoteFault>,
  timeout: Duration,
) -> Effect<T, RemoteFault, R>
where
  T: Clone + Send + 'static,
  R: Send + Sync + 'static,
{
  match sent {
    Ok(()) => reply.wait().timeout_fail(timeout, || RemoteFault::Timeout),
    Err(fault) => fail(fault),
  }
}

#[cfg(test)]
mod tests {
  use id_effect::Never;
  use id_effect::{Effect, succeed};
  use id_effect_process::{Next, NodeName, Process, ProcessCtx};

  use super::*;

  struct Idle;

  impl Process for Idle {
    type Args = ();
    type Env = ();
    type Call = ();
    type Reply = ();
    type Cast = ();
    type Info = ();
    type Error = Never;

    fn init(_: (), _: ProcessCtx) -> Effect<Self, Never, ()> {
      succeed(Idle)
    }

    fn handle_call(self, _: (), _: ProcessCtx) -> Effect<((), Next<Self>), Never, ()> {
      succeed(((), Next::Continue(self)))
    }
  }

  fn mesh(name: &str) -> Mesh {
    let mut registry = BehaviorRegistry::new();
    registry.register::<Idle>("idle");
    Mesh::new(
      NodeId::from_parts(name, 1),
      ProcessNode::new(NodeName::new(name)),
      registry,
    )
  }

  fn unit() -> Vec<u8> {
    postcard::to_allocvec(&()).expect("unit")
  }

  /// Run a request effect to its typed outcome.
  fn run<A: Send + 'static>(eff: Effect<A, RemoteFault>) -> Result<A, RemoteFault> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(f)) => Err(f),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  /// Peer sender that accepts and drops everything (an unresponsive node).
  fn black_hole() -> PeerSender {
    Arc::new(|_| Ok(()))
  }

  /// Peer sender for a node whose connection is gone.
  fn dead_link() -> PeerSender {
    Arc::new(|_| Err(RemoteFault::NoProc))
  }

  fn pending(mesh: &Mesh) -> (usize, usize) {
    (
      mesh.inner.pending_spawns.lock().expect("spawns").len(),
      mesh.inner.pending_calls.lock().expect("calls").len(),
    )
  }

  #[test]
  fn failed_send_leaves_no_pending_request() {
    let a = mesh("a@test");
    a.add_peer("z@test", dead_link());
    let target = Pid::from_parts(NodeName::new("z@test"), 1, 1);
    let wait = Duration::from_secs(5);
    assert_eq!(
      run(a.remote_spawn("z@test", "idle", unit(), None, wait)),
      Err(RemoteFault::NoProc)
    );
    assert_eq!(
      run(a.remote_call(&target, unit(), wait)),
      Err(RemoteFault::NoProc)
    );
    assert_eq!(pending(&a), (0, 0));
  }

  // A zero timeout expires at once, so the timeout path is exercised without waiting.
  #[test]
  fn timed_out_request_leaves_no_pending_entry() {
    let a = mesh("a@test");
    a.add_peer("z@test", black_hole());
    let target = Pid::from_parts(NodeName::new("z@test"), 1, 1);
    assert_eq!(
      run(a.remote_spawn("z@test", "idle", unit(), None, Duration::ZERO)),
      Err(RemoteFault::Timeout)
    );
    assert_eq!(
      run(a.remote_call(&target, unit(), Duration::ZERO)),
      Err(RemoteFault::Timeout)
    );
    assert_eq!(pending(&a), (0, 0));
  }

  /// A request is a `Send` fiber that can be interrupted while it waits: the interrupt ends
  /// it and its pending entry is forgotten.
  #[test]
  fn interrupted_request_forgets_its_pending_entry() {
    let a = mesh("a@test");
    let (sink, sent) = recorder();
    a.add_peer("z@test", sink);
    let target = Pid::from_parts(NodeName::new("z@test"), 1, 1);
    let hour = Duration::from_secs(3600);
    let spawn_eff = a.remote_spawn("z@test", "idle", unit(), None, hour);
    let call_eff = a.remote_call(&target, unit(), hour);
    fn assert_send<T: Send + 'static>(t: T) -> T {
      t
    }
    let rt = id_effect::Runtime::global();
    let spawn = assert_send(rt.fork(assert_send(spawn_eff)));
    let call = assert_send(rt.fork(assert_send(call_eff)));
    // Both requests reached the (silent) peer and are pending.
    for _ in 0..2 {
      sent
        .recv_timeout(Duration::from_secs(5))
        .expect("request sent");
    }
    assert_eq!(pending(&a), (1, 1));
    spawn.interrupt_now();
    call.interrupt_now();
    let interrupted = |c: &Cause<RemoteFault>| matches!(c, Cause::Interrupt(_));
    assert!(matches!(spawn.join().block_on(), Exit::Failure(c) if c.contains(&interrupted)));
    assert!(matches!(call.join().block_on(), Exit::Failure(c) if c.contains(&interrupted)));
    assert_eq!(pending(&a), (0, 0));
  }

  #[test]
  fn answered_request_leaves_no_pending_entry() {
    let fabric = MeshFabric::new();
    let a = mesh("a@test").with_fabric(fabric.clone());
    let _b = mesh("b@test").with_fabric(fabric);
    let pid =
      run(a.remote_spawn("b@test", "idle", unit(), None, Duration::from_secs(5))).expect("spawn");
    run(a.remote_call(&pid, unit(), Duration::from_secs(5))).expect("call");
    assert_eq!(pending(&a), (0, 0));
  }

  #[test]
  fn demonitor_removes_the_target_side_registration() {
    let fabric = MeshFabric::new();
    let a = mesh("a@test").with_fabric(fabric.clone());
    let b = mesh("b@test").with_fabric(fabric);
    let pid =
      run(a.remote_spawn("b@test", "idle", unit(), None, Duration::from_secs(5))).expect("spawn");
    let monitor = a.remote_monitor(&pid, Box::new(|_, _, _| {}));
    assert_eq!(
      b.inner.inbound_monitors.lock().expect("inbound").len(),
      1,
      "b tracks a's monitor under a's reference"
    );
    a.remote_demonitor(&pid, monitor);
    assert!(b.inner.inbound_monitors.lock().expect("inbound").is_empty());
    assert!(a.inner.remote_monitors.lock().expect("monitors").is_empty());
  }

  fn monitor_env(monitor: u64, target: &Pid) -> ProcessEnvelope {
    ProcessEnvelope::Monitor {
      monitor,
      target: PidWire::from(target),
    }
  }

  /// A peer sender that reports every `Down` it is asked to deliver.
  fn down_recorder() -> (PeerSender, flume::Receiver<(u64, PidWire)>) {
    let (tx, rx) = flume::unbounded();
    let sender: PeerSender = Arc::new(move |envelope| {
      if let ProcessEnvelope::Down {
        monitor, target, ..
      } = envelope
      {
        let _ = tx.send((monitor, target));
      }
      Ok(())
    });
    (sender, rx)
  }

  /// Regression: inbound monitors were keyed by origin *name*, so a
  /// restarted origin reusing a reference number overwrote its previous
  /// life's entry, and the stale sink later removed the new one.
  #[test]
  fn restarted_origin_does_not_collide_with_its_previous_life() {
    let b = mesh("b@test");
    let (sink, downs) = down_recorder();
    b.add_peer("a@test", sink);
    let p1 = b.spawn_local("idle", &unit(), None).expect("p1");
    let p2 = b.spawn_local("idle", &unit(), None).expect("p2");
    let old = NodeId::from_parts("a@test", 1);
    let new = NodeId::from_parts("a@test", 2);

    // Both lives use reference 7 (independent counters).
    b.handle_envelope_from(&old, monitor_env(7, &p1))
      .expect("old");
    b.handle_envelope_from(&new, monitor_env(7, &p2))
      .expect("new");
    assert_eq!(b.inner.inbound_monitors.lock().expect("inbound").len(), 2);

    // The old life's monitor fires for p1 — and only removes its own entry.
    b.process().exit(&p1, ExitReason::Killed);
    let (monitor, target) = downs.recv_timeout(Duration::from_secs(5)).expect("down");
    assert_eq!((monitor, target), (7, PidWire::from(&p1)));
    let inbound = b.inner.inbound_monitors.lock().expect("inbound");
    assert_eq!(
      inbound.keys().cloned().collect::<Vec<_>>(),
      vec![(Some(new.clone()), 7)]
    );
    drop(inbound);

    // The new life can still cancel its monitor.
    b.handle_envelope_from(
      &new,
      ProcessEnvelope::Demonitor {
        monitor: 7,
        target: PidWire::from(&p2),
      },
    )
    .expect("demonitor");
    assert!(b.inner.inbound_monitors.lock().expect("inbound").is_empty());
  }

  #[test]
  fn peer_down_purges_monitors_the_peer_held() {
    let b = mesh("b@test");
    b.add_peer("a@test", black_hole());
    b.add_peer("c@test", black_hole());
    let p = b.spawn_local("idle", &unit(), None).expect("p");
    let a1 = NodeId::from_parts("a@test", 1);
    let a2 = NodeId::from_parts("a@test", 2);
    let c = NodeId::from_parts("c@test", 1);
    b.handle_envelope_from(&a1, monitor_env(1, &p)).expect("a1");
    b.handle_envelope_from(&a2, monitor_env(2, &p)).expect("a2");
    b.handle_envelope_from(&c, monitor_env(3, &p)).expect("c");

    b.peer_down("a@test");

    let inbound = b.inner.inbound_monitors.lock().expect("inbound");
    assert_eq!(
      inbound.keys().cloned().collect::<Vec<_>>(),
      vec![(Some(c), 3)],
      "every incarnation of a is purged, c is kept"
    );
  }

  /// Runs `f` on a helper thread and fails (instead of hanging) if it does
  /// not finish — the symptom of a sink re-entering a held lock.
  fn finishes(f: impl FnOnce() + Send + 'static) -> bool {
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
      f();
      let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5)).is_ok()
  }

  /// Regression: `peer_down` invoked sinks while holding the monitor lock.
  #[test]
  fn sink_may_re_enter_the_mesh_from_peer_down() {
    let a = mesh("a@test");
    a.add_peer("z@test", black_hole());
    let target = Pid::from_parts(NodeName::new("z@test"), 1, 1);
    let reentrant = a.clone();
    let again = target.clone();
    a.remote_monitor(
      &target,
      Box::new(move |_, _, _| {
        // Both calls take the monitor lock that used to be held here.
        let m = reentrant.remote_monitor(&again, Box::new(|_, _, _| {}));
        reentrant.remote_demonitor(&again, m);
      }),
    );
    let mesh = a.clone();
    assert!(finishes(move || mesh.peer_down("z@test")), "deadlocked");
  }

  /// Regression: the `Down` handler invoked the sink under the monitor lock.
  #[test]
  fn sink_may_re_enter_the_mesh_from_a_down() {
    let fabric = MeshFabric::new();
    let a = mesh("a@test").with_fabric(fabric.clone());
    let b = mesh("b@test").with_fabric(fabric);
    let pid =
      run(a.remote_spawn("b@test", "idle", unit(), None, Duration::from_secs(5))).expect("spawn");
    let (tx, rx) = flume::bounded(1);
    let reentrant = Arc::clone(&a);
    a.remote_monitor(
      &pid,
      Box::new(move |_, who, _| {
        // The target is dead now, so this monitor fires immediately.
        reentrant.remote_monitor(&who, Box::new(|_, _, _| {}));
        let _ = tx.send(());
      }),
    );
    b.process().exit(&pid, ExitReason::Killed);
    assert!(
      rx.recv_timeout(Duration::from_secs(5)).is_ok(),
      "sink never completed (deadlock)"
    );
  }

  #[test]
  fn monitor_of_a_dead_target_leaves_no_inbound_entry() {
    let fabric = MeshFabric::new();
    let a = mesh("a@test").with_fabric(fabric.clone());
    let b = mesh("b@test").with_fabric(fabric);
    let ghost = Pid::from_parts(NodeName::new("b@test"), 9_999, 1);
    a.remote_monitor(&ghost, Box::new(|_, _, _| {}));
    assert!(b.inner.inbound_monitors.lock().expect("inbound").is_empty());
  }

  /// A peer sender that forwards every envelope it is asked to deliver.
  fn recorder() -> (PeerSender, flume::Receiver<ProcessEnvelope>) {
    let (tx, rx) = flume::unbounded();
    let sender: PeerSender = Arc::new(move |envelope| {
      let _ = tx.send(envelope);
      Ok(())
    });
    (sender, rx)
  }

  /// Exit reason of a local pid, observed through a raw monitor sink.
  fn exit_of(mesh: &Mesh, pid: &Pid) -> flume::Receiver<ExitReason> {
    let (tx, rx) = flume::unbounded();
    mesh.process().monitor_with_sink(
      pid,
      Box::new(move |_, _, reason| {
        let _ = tx.send(reason);
      }),
    );
    rx
  }

  #[test]
  fn inbound_link_forwards_local_exit_as_exit_signal() {
    let b = mesh("b@test");
    let (sink, sent) = recorder();
    b.add_peer("a@test", sink);
    let local = b.spawn_local("idle", &unit(), None).expect("local");
    let remote = Pid::from_parts(NodeName::new("a@test"), 5, 1);
    b.handle_envelope(ProcessEnvelope::Link {
      from: PidWire::from(&remote),
      to: PidWire::from(&local),
    })
    .expect("link");
    b.process().exit(&local, ExitReason::Killed);
    match sent
      .recv_timeout(Duration::from_secs(5))
      .expect("exit signal")
    {
      ProcessEnvelope::ExitSignal { from, to, reason } => {
        assert_eq!(from, PidWire::from(&local));
        assert_eq!(to, PidWire::from(&remote));
        assert_eq!(ExitReason::from(&reason), ExitReason::Killed);
      }
      _ => panic!("expected an ExitSignal"),
    }
  }

  #[test]
  fn link_and_exit_signal_for_foreign_pids_are_ignored() {
    let b = mesh("b@test");
    let (sink, sent) = recorder();
    b.add_peer("a@test", sink);
    let foreign = PidWire::from(&Pid::from_parts(NodeName::new("c@test"), 1, 1));
    let remote = PidWire::from(&Pid::from_parts(NodeName::new("a@test"), 1, 1));
    b.handle_envelope(ProcessEnvelope::Link {
      from: remote.clone(),
      to: foreign.clone(),
    })
    .expect("link");
    b.handle_envelope(ProcessEnvelope::Unlink {
      from: remote.clone(),
      to: foreign.clone(),
    })
    .expect("unlink");
    b.handle_envelope(ProcessEnvelope::ExitSignal {
      from: remote,
      to: foreign,
      reason: ExitReasonWire::from(&ExitReason::Killed),
    })
    .expect("exit signal");
    assert!(sent.try_recv().is_err(), "nothing is forwarded");
  }

  #[test]
  fn inbound_exit_signal_kills_the_local_target() {
    let b = mesh("b@test");
    let local = b.spawn_local("idle", &unit(), None).expect("local");
    let exit = exit_of(&b, &local);
    b.handle_envelope(ProcessEnvelope::ExitSignal {
      from: PidWire::from(&Pid::from_parts(NodeName::new("a@test"), 1, 1)),
      to: PidWire::from(&local),
      reason: ExitReasonWire::from(&ExitReason::Shutdown),
    })
    .expect("exit signal");
    assert_eq!(
      exit.recv_timeout(Duration::from_secs(5)).expect("exit"),
      ExitReason::Shutdown
    );
  }

  #[test]
  fn replies_to_an_unknown_origin_are_broadcast_to_every_peer() {
    let fabric = MeshFabric::new();
    let b = mesh("b@test").with_fabric(fabric.clone());
    let _c = mesh("c@test").with_fabric(fabric.clone());
    let (sink, sent) = recorder();
    b.add_peer("a@test", sink);
    // Through the fabric's sender-less path, so the spawn reply has no known origin.
    fabric
      .deliver(
        "b@test",
        ProcessEnvelope::Spawn {
          spawn_id: 41,
          behavior: "idle".into(),
          args: unit(),
          link_to: None,
        },
      )
      .expect("spawn");
    match sent.recv_timeout(Duration::from_secs(5)).expect("reply") {
      ProcessEnvelope::SpawnReply { spawn_id, result } => {
        assert_eq!(spawn_id, 41);
        assert!(result.is_ok(), "{result:?}");
      }
      _ => panic!("expected a SpawnReply"),
    }
    assert_eq!(
      fabric.deliver(
        "nowhere@test",
        ProcessEnvelope::Unlink {
          from: PidWire::from(&Pid::from_parts(NodeName::new("a@test"), 1, 1)),
          to: PidWire::from(&Pid::from_parts(NodeName::new("a@test"), 1, 1)),
        }
      ),
      Err(RemoteFault::NoProc)
    );
  }
}
