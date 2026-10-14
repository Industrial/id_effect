//! [`Mesh`] — process-layer distribution: remote spawn/send/call/link/monitor.
//!
//! Combines a local [`ProcessNode`], [`BehaviorRegistry`], and authenticated
//! [`Session`]s. Remote delivery is **at-most-once** (ADR 0009). On peer loss,
//! outstanding monitors fire [`ExitReason::NoConnection`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{Deferred, run_blocking};
use id_effect_process::{ExitReason, MonitorRef, MonitorSink, Pid, ProcessNode};

use crate::envelope::{
  ExitReasonWire, PidWire, ProcessEnvelope, RemoteFault, TraceContext, decode_envelope,
  encode_envelope,
};
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

/// Removes a pending request entry on every exit path (reply, send failure,
/// timeout). After a reply the entry is already gone and this is a no-op.
struct PendingEntry<'a, T> {
  map: &'a Mutex<HashMap<u64, T>>,
  id: u64,
}

impl<T> Drop for PendingEntry<'_, T> {
  fn drop(&mut self) {
    if let Ok(mut map) = self.map.lock() {
      map.remove(&self.id);
    }
  }
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
      }),
    }
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

  /// Register a peer sender (production session path).
  pub fn add_peer(&self, node_name: impl Into<String>, sender: PeerSender) {
    self
      .inner
      .peers
      .lock()
      .expect("peers")
      .insert(node_name.into(), sender);
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

  /// Remote spawn on `node_name`.
  pub fn remote_spawn(
    &self,
    node_name: &str,
    behavior: &str,
    args: Vec<u8>,
    link_to: Option<&Pid>,
    timeout: Duration,
  ) -> Result<Pid, RemoteFault> {
    if node_name == self.node_name() {
      return self.spawn_local(behavior, &args, link_to);
    }
    let spawn_id = self.alloc_id();
    let reply: Deferred<Pid, RemoteFault> =
      run_blocking(Deferred::make(), ()).unwrap_or_else(|_| unreachable!("Deferred::make"));
    self.inner.pending_spawns.lock().expect("spawns").insert(
      spawn_id,
      PendingSpawn {
        reply: reply.clone(),
      },
    );
    let _pending = PendingEntry {
      map: &self.inner.pending_spawns,
      id: spawn_id,
    };
    self.send_to(
      node_name,
      ProcessEnvelope::Spawn {
        spawn_id,
        behavior: behavior.to_string(),
        args,
        link_to: link_to.map(PidWire::from),
      },
    )?;
    self.await_deferred(reply, timeout)
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
  pub fn remote_spawn_placed(
    &self,
    reports: &[LoadReport],
    policy: &ClusterResourcePolicy,
    behavior: &str,
    args: Vec<u8>,
    link_to: Option<&Pid>,
    timeout: Duration,
  ) -> Result<Pid, RemoteFault> {
    let target = place(reports, policy).ok_or(RemoteFault::NoProc)?;
    self.remote_spawn(&target, behavior, args, link_to, timeout)
  }

  /// Remote cast (or local if same node).
  pub fn remote_cast(&self, to: &Pid, msg: Vec<u8>) -> Result<(), RemoteFault> {
    if to.node().as_str() == self.node_name() {
      return self.inner.registry.deliver_cast(to, &msg);
    }
    self.send_to(
      to.node().as_str(),
      ProcessEnvelope::Cast {
        to: PidWire::from(to),
        msg,
        trace: TraceContext::new(),
      },
    )
  }

  /// Remote info send.
  pub fn remote_info(&self, to: &Pid, msg: Vec<u8>) -> Result<(), RemoteFault> {
    if to.node().as_str() == self.node_name() {
      return self.inner.registry.deliver_info(to, &msg);
    }
    self.send_to(
      to.node().as_str(),
      ProcessEnvelope::Info {
        to: PidWire::from(to),
        msg,
        trace: TraceContext::new(),
      },
    )
  }

  /// Remote call with timeout.
  pub fn remote_call(
    &self,
    to: &Pid,
    msg: Vec<u8>,
    timeout: Duration,
  ) -> Result<Vec<u8>, RemoteFault> {
    if to.node().as_str() == self.node_name() {
      let reply: Deferred<Vec<u8>, RemoteFault> =
        run_blocking(Deferred::make(), ()).unwrap_or_else(|_| unreachable!("Deferred::make"));
      self.inner.registry.deliver_call(to, &msg, reply.clone())?;
      return self.await_deferred(reply, timeout);
    }
    let call_id = self.alloc_id();
    let reply: Deferred<Vec<u8>, RemoteFault> =
      run_blocking(Deferred::make(), ()).unwrap_or_else(|_| unreachable!("Deferred::make"));
    self.inner.pending_calls.lock().expect("calls").insert(
      call_id,
      PendingCall {
        reply: reply.clone(),
      },
    );
    let _pending = PendingEntry {
      map: &self.inner.pending_calls,
      id: call_id,
    };
    self.send_to(
      to.node().as_str(),
      ProcessEnvelope::Call {
        to: PidWire::from(to),
        call_id,
        msg,
        trace: TraceContext::new(),
      },
    )?;
    self.await_deferred(reply, timeout)
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
      ProcessEnvelope::Cast { to, msg, .. } => {
        let pid = to.to_pid();
        self.inner.registry.deliver_cast(&pid, &msg)
      }
      ProcessEnvelope::Info { to, msg, .. } => {
        let pid = to.to_pid();
        self.inner.registry.deliver_info(&pid, &msg)
      }
      ProcessEnvelope::Call {
        to, call_id, msg, ..
      } => {
        let pid = to.to_pid();
        let reply: Deferred<Vec<u8>, RemoteFault> =
          run_blocking(Deferred::make(), ()).unwrap_or_else(|_| unreachable!("Deferred::make"));
        let bridge = reply.clone();
        let mesh = self.clone();
        let origin = origin_name.map(str::to_string);
        if let Err(fault) = self.inner.registry.deliver_call(&pid, &msg, reply) {
          mesh.reply(
            origin.as_deref(),
            ProcessEnvelope::Reply {
              call_id,
              result: Err(fault),
            },
          );
          return Ok(());
        }
        std::thread::spawn(move || {
          let result = match run_blocking(bridge.wait(), ()) {
            Ok(bytes) => Ok(bytes),
            Err(id_effect::Cause::Fail(f)) => Err(f),
            Err(_) => Err(RemoteFault::Down(ExitReasonWire::Interrupted)),
          };
          mesh.reply(
            origin.as_deref(),
            ProcessEnvelope::Reply { call_id, result },
          );
        });
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
              let _ = pending.reply.try_fail_cause(id_effect::Cause::fail(f));
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
              let _ = pending.reply.try_fail_cause(id_effect::Cause::fail(f));
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

  fn await_deferred<T: Clone + Send + Sync + 'static>(
    &self,
    reply: Deferred<T, RemoteFault>,
    timeout: Duration,
  ) -> Result<T, RemoteFault> {
    let clock = self.inner.process.clock();
    let start = std::time::Instant::now();
    loop {
      if let Ok(Some(exit)) = run_blocking(reply.poll(), ()) {
        return match exit.into_result() {
          Ok(v) => Ok(v),
          Err(id_effect::Cause::Fail(f)) => Err(f),
          Err(_) => Err(RemoteFault::Down(ExitReasonWire::Interrupted)),
        };
      }
      if start.elapsed() >= timeout {
        return Err(RemoteFault::Timeout);
      }
      let _ = run_blocking(clock.sleep(Duration::from_millis(2)), ());
      std::thread::sleep(Duration::from_millis(2));
    }
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

#[cfg(test)]
mod tests {
  use id_effect::runtime::Never;
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
      ProcessNode::threads(NodeName::new(name)),
      registry,
    )
  }

  fn unit() -> Vec<u8> {
    postcard::to_allocvec(&()).expect("unit")
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
      a.remote_spawn("z@test", "idle", unit(), None, wait),
      Err(RemoteFault::NoProc)
    );
    assert_eq!(
      a.remote_call(&target, unit(), wait),
      Err(RemoteFault::NoProc)
    );
    assert_eq!(pending(&a), (0, 0));
  }

  // A zero timeout expires on the first check, so the timeout path is
  // exercised without waiting.
  #[test]
  fn timed_out_request_leaves_no_pending_entry() {
    let a = mesh("a@test");
    a.add_peer("z@test", black_hole());
    let target = Pid::from_parts(NodeName::new("z@test"), 1, 1);
    assert_eq!(
      a.remote_spawn("z@test", "idle", unit(), None, Duration::ZERO),
      Err(RemoteFault::Timeout)
    );
    assert_eq!(
      a.remote_call(&target, unit(), Duration::ZERO),
      Err(RemoteFault::Timeout)
    );
    assert_eq!(pending(&a), (0, 0));
  }

  #[test]
  fn answered_request_leaves_no_pending_entry() {
    let fabric = MeshFabric::new();
    let a = mesh("a@test").with_fabric(fabric.clone());
    let _b = mesh("b@test").with_fabric(fabric);
    let pid = a
      .remote_spawn("b@test", "idle", unit(), None, Duration::from_secs(5))
      .expect("spawn");
    a.remote_call(&pid, unit(), Duration::from_secs(5))
      .expect("call");
    assert_eq!(pending(&a), (0, 0));
  }

  #[test]
  fn demonitor_removes_the_target_side_registration() {
    let fabric = MeshFabric::new();
    let a = mesh("a@test").with_fabric(fabric.clone());
    let b = mesh("b@test").with_fabric(fabric);
    let pid = a
      .remote_spawn("b@test", "idle", unit(), None, Duration::from_secs(5))
      .expect("spawn");
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
    let pid = a
      .remote_spawn("b@test", "idle", unit(), None, Duration::from_secs(5))
      .expect("spawn");
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
