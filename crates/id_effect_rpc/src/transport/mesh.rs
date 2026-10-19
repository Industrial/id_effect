//! RPC over the process mesh (feature `mesh`; ADR 0014, design note §5 and §7): the contract an
//! [`RpcGroup`] serves over HTTP, callable node to node, postcard always.
//!
//! - **One endpoint process per service.** [`MeshRpc::serve`] makes a [`MeshEndpoint`]; started
//!   under a supervisor ([`MeshEndpoint::child_spec`]) or alone ([`MeshEndpoint::start`]), it is
//!   a mesh process registered under the cluster-wide name [`endpoint_name`]`(service)` (the
//!   0.11 global registry). A restarted endpoint registers the name again; callers resolve it
//!   per call, so a restart is transparent to them ([`crate::client::mesh`]).
//! - **Frames.** Every message is one design-note §5 [`Frame`] in the postcard encoding
//!   ([`encode_frame`]), cast to the endpoint (calls, client items, credits, cancels) or to the
//!   calling node's reply process (server items, credits, the exit). Only procedures whose
//!   schemas carry a typed codec (`WireSchema::with_serde`) are served; the others answer a
//!   `Rejected` exit with status 415.
//! - **Every kind.** Queries, mutations, server streams, client streams, two-way streams and
//!   subscriptions, with credit-based backpressure in both directions (initial window
//!   [`INITIAL_CREDITS`]); client streams within the procedure's item and byte caps.
//!
//! ## Who is calling
//!
//! The mesh delivers a cast to a process as bytes; it does not tell the process which node sent
//! them. [`MeshRpc::ingress`] does: it is the receiving side of the mesh connections (in place
//! of [`Mesh::handle_envelope_from`], [`MeshRpc::handle_session_frame`] in place of
//! [`Mesh::handle_session_frame`], [`MeshRpc::peer_sender`] for in-memory links), and stamps
//! every message for an RPC process with the node the authenticated session belongs to. The
//! stamp carries a 256-bit key that never leaves this node, so a message that bypassed the
//! ingress (or forged a stamp) is dropped: **a mesh RPC call is never run on an unverified
//! origin**.
//!
//! The verified node is the call's principal: the guards see a request head whose extensions are
//! [`MeshPeer`] (the node identity) and [`RpcPrincipal`]`("mesh:<node>")`. Nothing of the
//! calling node's own callers crosses: a procedure guarded for end users refuses mesh calls, and
//! a procedure meant for other nodes says so with [`MeshPeerGuard`].
//!
//! ## Limits and failures (all finite by default)
//!
//! - Calls open per calling node ≤ [`DEFAULT_MAX_CALLS_PER_PEER`], per endpoint ≤
//!   [`DEFAULT_MAX_CALLS`] (checked before any guard runs): over → `ResourceExhausted`.
//! - Deadlines: queries, mutations and client streams run under the caller's deadline clamped to
//!   [`MeshEndpoint::with_max_deadline`] (default 30 s).
//! - Frames are at most [`MAX_FRAME_BYTES`]; a peer that sends more items than it was granted
//!   credits gets `ResourceExhausted` and the call is dropped.
//! - A call is dropped (its fiber interrupted, finalizers run) when the caller cancels, when the
//!   calling node's reply process or the node itself goes down (a monitor), and when the
//!   endpoint process exits.
//! - Each call runs in the span `rpc.request` (attributes `rpc.service`, `rpc.method`,
//!   `rpc.procedure`, `mesh.node`, `mesh.from`, `mesh.parent`) under the caller's span, through
//!   the [`TracePropagator`] (dist.remote#8), and in a `tracing` span of the same name.
//!
//! ```rust
//! use id_effect_node::{BehaviorRegistry, Mesh, NodeId};
//! use id_effect_process::{NodeName, ProcessNode};
//! use id_effect_rpc::transport::mesh::{MeshRpc, endpoint_name};
//!
//! let mut registry = BehaviorRegistry::new();
//! MeshRpc::register_behavior(&mut registry); // before the mesh is built
//! let id = NodeId::from_parts("a@host", 1);
//! let mesh = Mesh::new(id.clone(), ProcessNode::new(NodeName::new(id.name())), registry);
//! let runtime = tokio::runtime::Runtime::new().unwrap();
//! let rpc = MeshRpc::on_runtime(id, mesh, runtime.handle().clone()).unwrap();
//! assert_eq!(rpc.mesh().node_name(), "a@host");
//! assert_eq!(endpoint_name("Chat"), "rpc:Chat");
//! ```

// The calling side (the reply process, client calls) is driven by `client::mesh` (feature
// `client`); a server-only build keeps it unused.
#![cfg_attr(not(feature = "client"), allow(dead_code))]

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use futures_util::StreamExt as _;
use futures_util::future::{Either, select};
use id_effect::{DynEnv, Effect, Never, fail, from_async, succeed, suspend, sync};
use id_effect_axum::route::GuardFuture;
use id_effect_node::{
  BehaviorRegistry, GlobalNameError, Mesh, NodeId, PeerSender, PidWire, ProcessEnvelope,
  ReliableMessage, RemoteFault, Session, TraceContext, TracePropagator, W3cTraceContext,
  decode_envelope, traceparent_in,
};
use id_effect_process::{
  ChildSpec, ExitReason, MonitorRef, Next, Pid, Process, ProcessCtx, StartError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};
use tracing::Instrument as _;

use crate::cancel::{CallControl, DEFAULT_MAX_DEADLINE, effective_deadline};
use crate::codec::postcard::{
  decode_client_payload, decode_frame, encode_frame, encode_server_frame,
};
use crate::codec::{CodecId, ProcedureCodecs};
use crate::contract::ProcedureKind;
use crate::handler::{CallContext, CallOutcome, InboundSender};
use crate::kinds::{CallFrames, exit_frame};
use crate::registry::{RouteGuard, RpcGroup};
use crate::server::RpcPrincipal;
use crate::server::dispatch::{self, Rejection};
use crate::wire::{CallId, Frame, FrameFlags, FrameKind, INITIAL_CREDITS, InputIssue, RpcFailure};

pub use crate::wire::MAX_FRAME_BYTES;

/// The behavior name of the RPC processes (endpoints and reply processes) in the mesh's
/// [`BehaviorRegistry`] ([`MeshRpc::register_behavior`]).
pub const BEHAVIOR: &str = "id_effect_rpc.mesh";

/// Prefix of an endpoint's cluster-wide name ([`endpoint_name`]).
pub const NAME_PREFIX: &str = "rpc:";

/// Prefix of the [`RpcPrincipal`] of a mesh call: `mesh:<node name>`.
pub const PRINCIPAL_PREFIX: &str = "mesh:";

/// Default cap on the calls one node has open on one endpoint.
pub const DEFAULT_MAX_CALLS_PER_PEER: usize = 256;

/// Default cap on the calls open on one endpoint.
pub const DEFAULT_MAX_CALLS: usize = 4096;

/// The `_tag` of the failure answered for a call refused before it ran: an unknown procedure
/// (`status` 404), a guard's refusal (its status, typically 401 or 403), a procedure without a
/// typed codec (415) or of a kind the transport does not call (400). Members: `status`,
/// `title`, `tag` (the procedure tag, when the refusal names it) — the shape the WebSocket
/// transport answers.
pub const REJECTED_TAG: &str = "Rejected";

/// Largest trace context (header bytes) a call may carry; a larger one is ignored.
pub const MAX_TRACE_BYTES: usize = 2048;

/// Items consumed between two credit grants.
const CREDIT_BATCH: u32 = INITIAL_CREDITS / 2;

/// Upper bound on the credits a peer may grant (larger grants are clamped).
const MAX_GRANTED_CREDITS: usize = 1 << 20;

/// The cluster-wide name of `service`'s endpoint: `rpc:<service>`.
pub fn endpoint_name(service: &str) -> String {
  format!("{NAME_PREFIX}{service}")
}

// ---------------------------------------------------------------------------------------------
// Who is calling
// ---------------------------------------------------------------------------------------------

/// The verified mesh node a call came from: a request extension of the head the procedure's
/// guards see on a mesh call (never present on HTTP requests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPeer(pub NodeId);

/// A guard that admits mesh calls (from any verified node, or from the named nodes only) and
/// refuses everything else: HTTP requests with 401, other nodes with 403.
#[derive(Debug, Clone, Default)]
pub struct MeshPeerGuard {
  nodes: Option<Vec<String>>,
}

impl MeshPeerGuard {
  /// Admit calls from every verified mesh node.
  pub fn any_node() -> Self {
    Self::default()
  }

  /// Admit calls from the nodes named `names` (`name@host`) only.
  pub fn nodes<S: Into<String>>(names: impl IntoIterator<Item = S>) -> Self {
    Self {
      nodes: Some(names.into_iter().map(Into::into).collect()),
    }
  }

  fn admits(&self, peer: &MeshPeer) -> bool {
    self
      .nodes
      .as_ref()
      .is_none_or(|nodes| nodes.iter().any(|n| n == peer.0.name()))
  }
}

impl RouteGuard for MeshPeerGuard {
  fn policy(&self) -> &str {
    "mesh-peer"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let decision = match parts.extensions.get::<MeshPeer>() {
      Some(peer) if self.admits(peer) => Ok(()),
      Some(_) => Err(StatusCode::FORBIDDEN.into_response()),
      None => Err(StatusCode::UNAUTHORIZED.into_response()),
    };
    Box::pin(async move { decision })
  }
}

/// The request head of a call from `origin`.
fn peer_parts(origin: &NodeId) -> Parts {
  let mut parts = axum::http::Request::new(()).into_parts().0;
  parts.extensions.insert(MeshPeer(origin.clone()));
  parts
    .extensions
    .insert(RpcPrincipal(format!("{PRINCIPAL_PREFIX}{}", origin.name())));
  parts
}

// ---------------------------------------------------------------------------------------------
// The message
// ---------------------------------------------------------------------------------------------

/// The ingress's proof of origin: the sending node and this node's private key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
  origin: NodeId,
  key: [u8; 32],
}

impl fmt::Debug for Stamp {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Stamp")
      .field("origin", &self.origin)
      .finish_non_exhaustive()
  }
}

/// One mesh RPC message: the cast payload of the RPC processes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MeshEnvelope {
  /// Set by the receiving node's ingress (or by this node for a local delivery), never trusted
  /// from the wire.
  stamp: Option<Stamp>,
  /// The sending RPC process (the reply process of a caller, the endpoint of a callee).
  from: PidWire,
  /// `call` frames: the caller's trace context.
  trace: TraceContext,
  /// `call` frames: the caller's deadline in milliseconds.
  deadline_ms: Option<u64>,
  /// One postcard [`Frame`].
  frame: Vec<u8>,
}

/// Constant-time key comparison.
fn same_key(a: &[u8; 32], b: &[u8; 32]) -> bool {
  a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn trace_bytes(trace: &TraceContext) -> usize {
  trace.iter().map(|(k, v)| k.len() + v.len()).sum()
}

// ---------------------------------------------------------------------------------------------
// The node handle
// ---------------------------------------------------------------------------------------------

/// Nodes by host key, for the RPC processes' `init` (process args cross a serde boundary).
static HOSTS: LazyLock<Mutex<HashMap<u128, Weak<Node>>>> = LazyLock::new(Mutex::default);

/// Why [`MeshRpc`] could not be set up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeshSetupError {
  /// The mesh's registry lacks [`BEHAVIOR`]: call [`MeshRpc::register_behavior`] on the
  /// registry before building the [`Mesh`].
  #[error("the mesh's behavior registry has no `{BEHAVIOR}` (MeshRpc::register_behavior)")]
  BehaviorNotRegistered,
  /// [`MeshRpc::new`] ran outside a tokio runtime.
  #[error("no tokio runtime (MeshRpc::on_runtime)")]
  NoRuntime,
  /// The node identity given is not the mesh's node.
  #[error("the node identity is not the mesh's node")]
  NodeMismatch,
}

/// Why an endpoint did not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeshServeError {
  /// The cluster-wide name is held by another process.
  #[error("{0}")]
  Name(#[from] GlobalNameError),
}

#[derive(Clone)]
enum Target {
  Endpoint(Arc<Endpoint>),
  Reply,
}

/// What reaches a client call.
pub(crate) enum ClientEvent {
  /// A frame from the endpoint.
  Frame(Frame<Vec<u8>>),
  /// The endpoint (or the reply process) exited.
  Down(ExitReason),
  /// The endpoint broke the protocol (items beyond the granted credits).
  Violation(&'static str),
}

struct ClientSlot {
  endpoint: Pid,
  tx: mpsc::UnboundedSender<ClientEvent>,
  in_credits: Arc<AtomicU32>,
  out_credits: Arc<Semaphore>,
}

pub(crate) struct Node {
  host: u128,
  id: NodeId,
  mesh: Mesh,
  key: [u8; 32],
  runtime: tokio::runtime::Handle,
  propagator: Mutex<Arc<dyn TracePropagator>>,
  targets: Mutex<HashMap<Pid, Target>>,
  reply: Mutex<Option<Pid>>,
  calls: Mutex<HashMap<u64, ClientSlot>>,
  next_call: AtomicU64,
}

impl Drop for Node {
  fn drop(&mut self) {
    if let Ok(mut hosts) = HOSTS.lock() {
      hosts.remove(&self.host);
    }
  }
}

/// RPC over one node's [`Mesh`]: the ingress that verifies who is calling, the endpoints this
/// node serves ([`MeshRpc::serve`]) and the calls it makes ([`MeshRpc::client`]).
#[derive(Clone)]
pub struct MeshRpc {
  pub(crate) node: Arc<Node>,
}

impl fmt::Debug for MeshRpc {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MeshRpc")
      .field("node", &self.node.mesh.node_name())
      .finish_non_exhaustive()
  }
}

impl MeshRpc {
  /// Register the RPC process behavior in `registry`. Call it before building the [`Mesh`]
  /// (the mesh owns its registry).
  pub fn register_behavior(registry: &mut BehaviorRegistry) {
    registry.register::<RpcProcess>(BEHAVIOR);
  }

  /// RPC over `mesh` (whose node is `id`), running calls on the current tokio runtime.
  ///
  /// # Errors
  ///
  /// [`MeshSetupError::NoRuntime`] outside a runtime; as [`MeshRpc::on_runtime`].
  pub fn new(id: NodeId, mesh: Mesh) -> Result<Self, MeshSetupError> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| MeshSetupError::NoRuntime)?;
    Self::on_runtime(id, mesh, runtime)
  }

  /// RPC over `mesh`, running calls on `runtime`.
  ///
  /// # Errors
  ///
  /// [`MeshSetupError::BehaviorNotRegistered`] when the mesh's registry lacks [`BEHAVIOR`];
  /// [`MeshSetupError::NodeMismatch`] when `id` does not name the mesh's node.
  pub fn on_runtime(
    id: NodeId,
    mesh: Mesh,
    runtime: tokio::runtime::Handle,
  ) -> Result<Self, MeshSetupError> {
    if mesh.registry().get(BEHAVIOR).is_none() {
      return Err(MeshSetupError::BehaviorNotRegistered);
    }
    if id.name() != mesh.node_name() {
      return Err(MeshSetupError::NodeMismatch);
    }
    let host = rand::random::<u128>();
    let node = Arc::new(Node {
      host,
      id,
      mesh,
      key: rand::random(),
      runtime,
      propagator: Mutex::new(Arc::new(W3cTraceContext::new())),
      targets: Mutex::default(),
      reply: Mutex::default(),
      calls: Mutex::default(),
      next_call: AtomicU64::new(1),
    });
    HOSTS
      .lock()
      .expect("hosts")
      .insert(host, Arc::downgrade(&node));
    Ok(Self { node })
  }

  /// Carry trace context with `propagator` (default [`W3cTraceContext`], which opens receiving
  /// spans only under the hook it is given).
  #[must_use]
  pub fn with_trace_propagator(self, propagator: Arc<dyn TracePropagator>) -> Self {
    *self.node.propagator.lock().expect("propagator") = propagator;
    self
  }

  /// The mesh.
  pub fn mesh(&self) -> &Mesh {
    &self.node.mesh
  }

  /// Handle an envelope that the authenticated session with `origin` delivered: stamp the
  /// messages for this node's RPC processes with `origin`, then hand it to
  /// [`Mesh::handle_envelope_from`].
  ///
  /// # Errors
  ///
  /// As [`Mesh::handle_envelope_from`].
  pub fn ingress(&self, origin: &NodeId, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    let envelope = self.node.stamp_envelope(origin, envelope);
    self.node.mesh.handle_envelope_from(origin, envelope)
  }

  /// Pump one inbound frame of `session` through [`MeshRpc::ingress`] (the RPC-aware
  /// [`Mesh::handle_session_frame`]).
  ///
  /// # Errors
  ///
  /// As [`Mesh::handle_session_frame`]: a dead session or a closed one ([`RemoteFault::NoProc`]),
  /// an undecodable frame.
  pub async fn handle_session_frame(&self, session: &Session) -> Result<(), RemoteFault> {
    let frame = session
      .recv()
      .await
      .map_err(|e| RemoteFault::Decode(e.to_string()))?;
    match frame {
      id_effect_node::Frame::Data(bytes) => {
        let envelope = decode_envelope(&bytes).map_err(|e| RemoteFault::Decode(e.to_string()))?;
        self.ingress(session.peer(), envelope)
      }
      id_effect_node::Frame::Close(_) => Err(RemoteFault::NoProc),
      _ => Ok(()),
    }
  }

  /// The [`PeerSender`] by which another node, `origin`, delivers into this node in process
  /// (tests, in-memory clusters): every envelope goes through [`MeshRpc::ingress`].
  pub fn peer_sender(&self, origin: NodeId) -> PeerSender {
    let node = Arc::downgrade(&self.node);
    Arc::new(move |envelope| match node.upgrade() {
      Some(node) => MeshRpc { node }.ingress(&origin, envelope),
      None => Err(RemoteFault::NoProc),
    })
  }

  /// Serve `group` as `service` on this node; [`MeshEndpoint::start`] or
  /// [`MeshEndpoint::child_spec`] runs it. Every call runs with `env`.
  pub fn serve(&self, service: impl Into<String>, group: RpcGroup, env: DynEnv) -> MeshEndpoint {
    MeshEndpoint {
      rpc: self.clone(),
      service: service.into(),
      group: Arc::new(group),
      env,
      config: ServeConfig::default(),
      shared: std::sync::OnceLock::new(),
    }
  }
}

impl Node {
  /// Run `task` on this node's runtime.
  pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
    drop(self.runtime.spawn(task));
  }

  pub(crate) fn propagator(&self) -> Arc<dyn TracePropagator> {
    Arc::clone(&self.propagator.lock().expect("propagator"))
  }

  fn is_target(&self, pid: &Pid) -> bool {
    self.targets.lock().expect("targets").contains_key(pid)
  }

  fn stamp_envelope(&self, origin: &NodeId, envelope: ProcessEnvelope) -> ProcessEnvelope {
    match envelope {
      ProcessEnvelope::Cast { to, msg, trace } => {
        let msg = self.stamp_msg(origin, &to, msg);
        ProcessEnvelope::Cast { to, msg, trace }
      }
      ProcessEnvelope::Reliable {
        seq,
        floor,
        message: ReliableMessage::Cast { to, msg, trace },
      } => {
        let msg = self.stamp_msg(origin, &to, msg);
        ProcessEnvelope::Reliable {
          seq,
          floor,
          message: ReliableMessage::Cast { to, msg, trace },
        }
      }
      other => other,
    }
  }

  /// `msg` for `to` with the stamp of `origin` when `to` is one of this node's RPC processes
  /// (any stamp the sender wrote is replaced); other messages pass untouched.
  fn stamp_msg(&self, origin: &NodeId, to: &PidWire, msg: Vec<u8>) -> Vec<u8> {
    if !self.is_target(&to.to_pid()) {
      return msg;
    }
    match postcard::from_bytes::<MeshEnvelope>(&msg) {
      Ok(mut envelope) => {
        envelope.stamp = Some(Stamp {
          origin: origin.clone(),
          key: self.key,
        });
        postcard::to_allocvec(&envelope).unwrap_or_default()
      }
      // Undecodable: the registry refuses it as it would without the ingress.
      Err(_) => msg,
    }
  }

  fn verified(&self, envelope: &MeshEnvelope) -> Option<NodeId> {
    let stamp = envelope.stamp.as_ref()?;
    same_key(&stamp.key, &self.key).then(|| stamp.origin.clone())
  }

  /// Cast `envelope` to the RPC process `to` (stamped here for a local delivery).
  fn send(&self, to: &Pid, mut envelope: MeshEnvelope) -> Result<(), RemoteFault> {
    envelope.stamp = (to.node().as_str() == self.mesh.node_name()).then(|| Stamp {
      origin: self.id.clone(),
      key: self.key,
    });
    let bytes = postcard::to_allocvec(&envelope).map_err(|e| RemoteFault::Decode(e.to_string()))?;
    self.mesh.remote_cast(to, bytes)
  }

  /// Spawn an RPC process for `target` (linked to `link`), routable from the mesh.
  fn spawn_target(self: &Arc<Self>, link: Option<&Pid>, target: Target) -> Pid {
    let process = self.mesh.process();
    let args = RpcProcessArgs { host: self.host };
    let addr = match link {
      Some(parent) => process.spawn_linked_to::<RpcProcess>(parent, args, ()),
      None => process.spawn::<RpcProcess>(args, ()),
    };
    let pid = addr.pid().clone();
    self
      .targets
      .lock()
      .expect("targets")
      .insert(pid.clone(), target);
    self
      .mesh
      .registry()
      .adopt_local::<RpcProcess>(BEHAVIOR, &pid, addr.clone_mailbox());
    let node = Arc::downgrade(self);
    process.monitor_with_sink(
      &pid,
      Box::new(move |_monitor, dead, reason| {
        if let Some(node) = node.upgrade() {
          node.target_exited(&dead, &reason);
        }
      }),
    );
    pid
  }

  fn target_exited(self: &Arc<Self>, pid: &Pid, reason: &ExitReason) {
    let target = self.targets.lock().expect("targets").remove(pid);
    self.mesh.registry().forget(pid);
    match target {
      Some(Target::Endpoint(endpoint)) => endpoint.drop_generation(self, pid),
      Some(Target::Reply) => {
        let mut reply = self.reply.lock().expect("reply");
        if reply.as_ref() == Some(pid) {
          *reply = None;
        }
        drop(reply);
        let slots: Vec<_> = self
          .calls
          .lock()
          .expect("calls")
          .values()
          .map(|slot| slot.tx.clone())
          .collect();
        for tx in slots {
          let _ = tx.send(ClientEvent::Down(reason.clone()));
        }
      }
      None => {}
    }
  }

  /// A message for this node's RPC process `me`.
  fn receive(self: &Arc<Self>, me: &Pid, envelope: MeshEnvelope) {
    let Some(origin) = self.verified(&envelope) else {
      tracing::warn!(
        target: "id_effect_rpc::mesh",
        "dropped a mesh RPC message without a verified origin (deliver through MeshRpc::ingress)"
      );
      return;
    };
    if envelope.from.node != origin.name() {
      tracing::warn!(target: "id_effect_rpc::mesh", "dropped a mesh RPC message naming another node");
      return;
    }
    let target = self.targets.lock().expect("targets").get(me).cloned();
    match target {
      Some(Target::Endpoint(endpoint)) => endpoint.receive(self, me, origin, envelope),
      Some(Target::Reply) => self.client_receive(envelope),
      None => {}
    }
  }

  // --- the client side -----------------------------------------------------------------------

  /// This node's reply process (started on first use).
  fn reply_pid(self: &Arc<Self>) -> Pid {
    let mut reply = self.reply.lock().expect("reply");
    if let Some(pid) = reply.as_ref()
      && self.is_target(pid)
    {
      return pid.clone();
    }
    let pid = self.spawn_target(None, Target::Reply);
    *reply = Some(pid.clone());
    pid
  }

  fn client_receive(&self, envelope: MeshEnvelope) {
    let Ok(frame) = decode_frame(&envelope.frame) else {
      return;
    };
    let calls = self.calls.lock().expect("calls");
    let Some(slot) = calls.get(&frame.id.get()) else {
      return;
    };
    if envelope.from != PidWire::from(&slot.endpoint) {
      return;
    }
    match frame.kind {
      FrameKind::Credit => {
        grant(&slot.out_credits, frame.credits.unwrap_or(0));
        return;
      }
      FrameKind::Item if !take_one(&slot.in_credits) => {
        slot.out_credits.close();
        let _ = slot
          .tx
          .send(ClientEvent::Violation("items beyond the granted credits"));
        return;
      }
      FrameKind::Exit => slot.out_credits.close(),
      _ => {}
    }
    let _ = slot.tx.send(ClientEvent::Frame(frame));
  }

  fn client_event(&self, id: u64, event: ClientEvent) {
    if let Some(slot) = self.calls.lock().expect("calls").get(&id) {
      slot.out_credits.close();
      let _ = slot.tx.send(event);
    }
  }

  /// Open a call to `endpoint`: its reply slot and a monitor on the endpoint. `items` is
  /// whether the endpoint sends items (the initial credit window).
  pub(crate) fn open_call(
    self: &Arc<Self>,
    endpoint: Pid,
    items: bool,
  ) -> Result<OpenCall, RemoteFault> {
    let reply = self.reply_pid();
    let id = CallId::new(self.next_call.fetch_add(1, Ordering::Relaxed))
      .map_err(|e| RemoteFault::Decode(e.to_string()))?;
    let (tx, rx) = mpsc::unbounded_channel();
    let in_credits = Arc::new(AtomicU32::new(if items { INITIAL_CREDITS } else { 0 }));
    let out_credits = Arc::new(Semaphore::new(INITIAL_CREDITS as usize));
    self.calls.lock().expect("calls").insert(
      id.get(),
      ClientSlot {
        endpoint: endpoint.clone(),
        tx,
        in_credits: Arc::clone(&in_credits),
        out_credits: Arc::clone(&out_credits),
      },
    );
    let node = Arc::downgrade(self);
    let key = id.get();
    let monitor = self.mesh.remote_monitor(
      &endpoint,
      Box::new(move |_monitor, _pid, reason| {
        if let Some(node) = node.upgrade() {
          node.client_event(key, ClientEvent::Down(reason));
        }
      }),
    );
    Ok(OpenCall {
      shared: Arc::new(CallShared {
        node: Arc::clone(self),
        id,
        endpoint,
        reply,
        in_credits,
        out_credits,
        finished: AtomicBool::new(false),
        ended: AtomicBool::new(false),
        monitor: Mutex::new(Some(monitor)),
      }),
      rx,
    })
  }
}

/// Take one credit from `credits`; `false` when none is left. Each counter has one taker (the
/// RPC process that receives the items, one message at a time), so a check then a decrement is
/// exact; grants only ever add.
fn take_one(credits: &AtomicU32) -> bool {
  let available = credits.load(Ordering::SeqCst) > 0;
  if available {
    credits.fetch_sub(1, Ordering::SeqCst);
  }
  available
}

fn grant(credits: &Semaphore, n: u32) {
  let room = MAX_GRANTED_CREDITS.saturating_sub(credits.available_permits());
  credits.add_permits((n as usize).min(room));
}

/// What a client call shares between its reading half and its sink.
pub(crate) struct CallShared {
  node: Arc<Node>,
  id: CallId,
  endpoint: Pid,
  reply: Pid,
  in_credits: Arc<AtomicU32>,
  out_credits: Arc<Semaphore>,
  finished: AtomicBool,
  ended: AtomicBool,
  monitor: Mutex<Option<MonitorRef>>,
}

impl CallShared {
  /// The call id.
  pub(crate) fn id(&self) -> CallId {
    self.id
  }

  /// Whether the call is over (exit, endpoint down, cancelled).
  pub(crate) fn is_finished(&self) -> bool {
    self.finished.load(Ordering::SeqCst)
  }

  pub(crate) fn finish(&self) {
    self.finished.store(true, Ordering::SeqCst);
    self.out_credits.close();
  }

  /// Send `frame` to the endpoint.
  pub(crate) fn send(
    &self,
    frame: &Frame<Vec<u8>>,
    trace: TraceContext,
    deadline: Option<Duration>,
  ) -> Result<(), RemoteFault> {
    let bytes = encode_frame(frame).map_err(|e| RemoteFault::Decode(e.to_string()))?;
    self.node.send(
      &self.endpoint,
      MeshEnvelope {
        stamp: None,
        from: PidWire::from(&self.reply),
        trace,
        deadline_ms: deadline.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        frame: bytes,
      },
    )
  }

  /// Wait for one credit to send an item; `false` when the call is over.
  pub(crate) async fn take_credit(&self) -> bool {
    match self.out_credits.acquire().await {
      Ok(permit) => {
        permit.forget();
        !self.is_finished()
      }
      Err(_) => false,
    }
  }

  /// Accept `n` more items and tell the endpoint.
  pub(crate) fn grant(&self, n: u32) {
    self.in_credits.fetch_add(n, Ordering::SeqCst);
    let _ = self.send(&Frame::credit(self.id, n), TraceContext::new(), None);
  }

  /// Half-close: no more items from this side (once).
  pub(crate) fn end(&self) -> Result<(), RemoteFault> {
    if self.ended.swap(true, Ordering::SeqCst) || self.is_finished() {
      return Ok(());
    }
    self.send(
      &Frame::new(self.id, FrameKind::End).with_flags(FrameFlags::END),
      TraceContext::new(),
      None,
    )
  }
}

/// A client call: the shared half and the events from the endpoint. Dropping it before the
/// exit cancels the call.
pub(crate) struct OpenCall {
  pub(crate) shared: Arc<CallShared>,
  pub(crate) rx: mpsc::UnboundedReceiver<ClientEvent>,
}

impl Drop for OpenCall {
  fn drop(&mut self) {
    let shared = &self.shared;
    if !shared.is_finished() {
      let _ = shared.send(
        &Frame::new(shared.id, FrameKind::Cancel),
        TraceContext::new(),
        None,
      );
    }
    shared.finish();
    shared
      .node
      .calls
      .lock()
      .expect("calls")
      .remove(&shared.id.get());
    let monitor = shared.monitor.lock().expect("monitor").take();
    if let Some(monitor) = monitor {
      shared.node.mesh.remote_demonitor(&shared.endpoint, monitor);
    }
  }
}

// ---------------------------------------------------------------------------------------------
// The RPC process
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcProcessArgs {
  host: u128,
}

/// An endpoint or reply process: a named, supervisable mailbox in front of its [`Node`].
struct RpcProcess {
  node: Weak<Node>,
}

impl Process for RpcProcess {
  type Args = RpcProcessArgs;
  type Env = ();
  type Call = ();
  type Reply = ();
  type Cast = MeshEnvelope;
  type Info = ();
  type Error = String;

  fn init(args: RpcProcessArgs, _ctx: ProcessCtx) -> Effect<Self, String, ()> {
    suspend(move || {
      let node = HOSTS
        .lock()
        .expect("hosts")
        .get(&args.host)
        .cloned()
        .filter(|node| node.strong_count() > 0);
      match node {
        Some(node) => succeed(Self { node }),
        None => fail("no MeshRpc for this process".to_owned()),
      }
    })
  }

  fn handle_call(self, (): (), _ctx: ProcessCtx) -> Effect<((), Next<Self>), String, ()> {
    succeed(((), Next::Continue(self)))
  }

  fn handle_cast(self, msg: MeshEnvelope, ctx: ProcessCtx) -> Effect<Next<Self>, String, ()> {
    sync(move || match self.node.upgrade() {
      Some(node) => {
        node.receive(ctx.pid(), msg);
        Next::Continue(self)
      }
      None => Next::Stop(ExitReason::Normal, self),
    })
  }
}

// ---------------------------------------------------------------------------------------------
// The endpoint
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ServeConfig {
  max_deadline: Duration,
  max_calls_per_peer: usize,
  max_calls: usize,
}

impl Default for ServeConfig {
  fn default() -> Self {
    Self {
      max_deadline: DEFAULT_MAX_DEADLINE,
      max_calls_per_peer: DEFAULT_MAX_CALLS_PER_PEER,
      max_calls: DEFAULT_MAX_CALLS,
    }
  }
}

/// A service served over the mesh ([`MeshRpc::serve`]); run it with [`MeshEndpoint::start`] or
/// under a supervisor with [`MeshEndpoint::child_spec`].
pub struct MeshEndpoint {
  rpc: MeshRpc,
  service: String,
  group: Arc<RpcGroup>,
  env: DynEnv,
  config: ServeConfig,
  shared: std::sync::OnceLock<Arc<Endpoint>>,
}

impl fmt::Debug for MeshEndpoint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MeshEndpoint")
      .field("service", &self.service)
      .field("procedures", &self.group.len())
      .field("config", &self.config)
      .finish_non_exhaustive()
  }
}

impl MeshEndpoint {
  /// Cap every call's deadline at `max` (queries, mutations, client streams).
  #[must_use]
  pub fn with_max_deadline(mut self, max: Duration) -> Self {
    self.config.max_deadline = max;
    self
  }

  /// Let one node have at most `calls` (≥ 1) calls open on this endpoint.
  #[must_use]
  pub fn with_max_calls_per_peer(mut self, calls: usize) -> Self {
    self.config.max_calls_per_peer = calls.max(1);
    self
  }

  /// Let this endpoint have at most `calls` (≥ 1) calls open.
  #[must_use]
  pub fn with_max_calls(mut self, calls: usize) -> Self {
    self.config.max_calls = calls.max(1);
    self
  }

  /// The service name.
  pub fn service(&self) -> &str {
    &self.service
  }

  /// The cluster-wide name the endpoint registers ([`endpoint_name`]).
  pub fn name(&self) -> String {
    endpoint_name(&self.service)
  }

  /// Calls open on the endpoint now.
  pub fn open_calls(&self) -> usize {
    self
      .shared
      .get()
      .map_or(0, |shared| shared.calls.lock().expect("calls").len())
  }

  fn shared(&self) -> Arc<Endpoint> {
    Arc::clone(self.shared.get_or_init(|| {
      Arc::new(Endpoint {
        service: self.service.clone(),
        group: Arc::clone(&self.group),
        env: self.env.clone(),
        config: self.config,
        calls: Mutex::default(),
      })
    }))
  }

  /// Start the endpoint process (unsupervised) and register its name.
  ///
  /// # Errors
  ///
  /// [`MeshServeError::Name`] when the name is held by a live process of another node.
  pub fn start(&self) -> Result<Pid, MeshServeError> {
    start_endpoint(&self.rpc.node, &self.shared(), None)
  }

  /// The supervisor child that runs the endpoint (id: its name, restart `Permanent`): every
  /// start spawns the process linked to the supervisor and registers the name again.
  pub fn child_spec(&self) -> ChildSpec {
    let node = Arc::clone(&self.rpc.node);
    let shared = self.shared();
    ChildSpec::from_start(
      self.name(),
      Arc::new(move |_process, supervisor| {
        start_endpoint(&node, &shared, Some(supervisor))
          .map_err(|e| StartError::Failed(e.to_string()))
      }),
    )
  }
}

fn start_endpoint(
  node: &Arc<Node>,
  endpoint: &Arc<Endpoint>,
  link: Option<&Pid>,
) -> Result<Pid, MeshServeError> {
  let pid = node.spawn_target(link, Target::Endpoint(Arc::clone(endpoint)));
  let name = endpoint_name(&endpoint.service);
  let global = node.mesh.global();
  let registered = match global.register(name.clone(), &pid) {
    // A previous life of this endpoint on this node still holds it: the name is ours.
    Err(GlobalNameError::AlreadyRegistered { owner, .. })
      if owner.node().as_str() == node.mesh.node_name() && !node.is_target(&owner) =>
    {
      let _ = global.unregister(&name);
      global.register(name, &pid)
    }
    other => other,
  };
  match registered {
    Ok(()) => Ok(pid),
    Err(e) => {
      node.mesh.process().exit(&pid, ExitReason::Shutdown);
      Err(e.into())
    }
  }
}

/// A server call's key: the caller's reply process and the call id.
type CallKey = (Pid, CallId);

enum Inbound {
  Item(Vec<u8>),
  End,
}

struct ServerCall {
  /// The endpoint process that runs it.
  generation: Pid,
  origin: String,
  codecs: ProcedureCodecs,
  inbound: Option<mpsc::UnboundedSender<Inbound>>,
  in_credits: Arc<AtomicU32>,
  out_credits: Arc<Semaphore>,
  task: Option<tokio::task::AbortHandle>,
  monitor: Option<MonitorRef>,
}

struct Endpoint {
  service: String,
  group: Arc<RpcGroup>,
  env: DynEnv,
  config: ServeConfig,
  calls: Mutex<HashMap<CallKey, ServerCall>>,
}

/// The frames of one server call back to the caller.
struct Reply {
  node: Arc<Node>,
  to: Pid,
  from: Pid,
  id: CallId,
  codecs: ProcedureCodecs,
  cid: Arc<str>,
}

impl Reply {
  fn raw(&self, frame: Vec<u8>) -> bool {
    self
      .node
      .send(
        &self.to,
        MeshEnvelope {
          stamp: None,
          from: PidWire::from(&self.from),
          trace: TraceContext::new(),
          deadline_ms: None,
          frame,
        },
      )
      .is_ok()
  }

  /// Send a server frame; one whose payload does not encode ends the call with
  /// `InternalError`. `false` when the call is over.
  fn frame(&self, frame: &Frame) -> bool {
    match encode_server_frame(&self.codecs, frame) {
      Ok(bytes) => self.raw(bytes) && frame.kind != FrameKind::Exit,
      Err(_) => {
        self.exit(&CallOutcome::internal(&self.cid));
        false
      }
    }
  }

  fn exit(&self, outcome: &CallOutcome) {
    let bytes = encode_server_frame(&self.codecs, &exit_frame(self.id, outcome)).or_else(|_| {
      encode_server_frame(
        &ProcedureCodecs::default(),
        &exit_frame(self.id, &CallOutcome::internal(&self.cid)),
      )
    });
    if let Ok(bytes) = bytes {
      self.raw(bytes);
    }
  }

  fn credit(&self, n: u32) {
    if let Ok(bytes) = encode_frame(&Frame::credit(self.id, n)) {
      self.raw(bytes);
    }
  }

  /// Forward a running stream's frames, an item per credit.
  async fn pump(&self, mut frames: CallFrames, credits: &Semaphore) {
    while let Some(frame) = frames.next().await {
      if frame.kind == FrameKind::Item {
        // The server's credits are never closed: this waits until the caller grants one.
        let _ = credits
          .acquire()
          .await
          .map(tokio::sync::SemaphorePermit::forget);
      }
      if !self.frame(&frame) {
        return;
      }
    }
  }
}

fn rejected(status: StatusCode, tag: Option<&str>) -> CallOutcome {
  let mut error = json!({
    "_tag": REJECTED_TAG,
    "status": status.as_u16(),
    "title": status.canonical_reason().unwrap_or(REJECTED_TAG),
  });
  if let Some(tag) = tag {
    error["tag"] = Value::String(tag.to_owned());
  }
  CallOutcome::typed(error, status)
}

fn rejection_outcome(rejection: &Rejection) -> CallOutcome {
  match rejection {
    Rejection::UnknownProcedure { tag, .. } => rejected(StatusCode::NOT_FOUND, Some(tag)),
    Rejection::Refused(response) => rejected(response.status(), None),
    Rejection::WrongKind { tag, .. } => rejected(StatusCode::BAD_REQUEST, Some(tag)),
  }
}

fn invalid_input(message: String) -> RpcFailure {
  RpcFailure::InvalidInput {
    issues: vec![InputIssue {
      path: String::new(),
      message,
    }],
  }
}

fn exhausted(limit: &str) -> RpcFailure {
  RpcFailure::ResourceExhausted {
    limit: limit.to_owned(),
  }
}

/// Everything a server call starts from.
struct CallStart {
  kind: ProcedureKind,
  tag: String,
  version: Option<u32>,
  payload: Option<Vec<u8>>,
  frame_cursor: Option<crate::wire::Cursor>,
  origin: NodeId,
  deadline: Option<Duration>,
  inbound: Option<mpsc::UnboundedReceiver<Inbound>>,
  in_credits: Arc<AtomicU32>,
  out_credits: Arc<Semaphore>,
  caps: (u64, u64),
}

impl Endpoint {
  fn receive(self: &Arc<Self>, node: &Arc<Node>, me: &Pid, origin: NodeId, envelope: MeshEnvelope) {
    let Ok(frame) = decode_frame(&envelope.frame) else {
      tracing::debug!(target: "id_effect_rpc::mesh", "dropped an undecodable frame");
      return;
    };
    let key = (envelope.from.to_pid(), frame.id);
    match frame.kind {
      FrameKind::Call => self.open(node, me, origin, key, frame, envelope),
      FrameKind::Item => {
        let end = frame.flags.contains(FrameFlags::END);
        self.inbound(node, &key, Inbound::Item(frame.payload.unwrap_or_default()));
        if end {
          self.inbound(node, &key, Inbound::End);
        }
      }
      FrameKind::End => self.inbound(node, &key, Inbound::End),
      FrameKind::Credit => {
        let calls = self.calls.lock().expect("calls");
        if let Some(call) = calls.get(&key) {
          grant(&call.out_credits, frame.credits.unwrap_or(0));
        }
      }
      FrameKind::Cancel => self.drop_call(node, &key),
      _ => {}
    }
  }

  fn open(
    self: &Arc<Self>,
    node: &Arc<Node>,
    me: &Pid,
    origin: NodeId,
    key: CallKey,
    frame: Frame<Vec<u8>>,
    envelope: MeshEnvelope,
  ) {
    let cid: Arc<str> = Arc::from(format!("mesh-{}", uuid::Uuid::new_v4()));
    let mut reply = Reply {
      node: Arc::clone(node),
      to: key.0.clone(),
      from: me.clone(),
      id: frame.id,
      codecs: ProcedureCodecs::default(),
      cid: Arc::clone(&cid),
    };
    let tag = frame.tag.clone().unwrap_or_default();
    let entry = match dispatch::resolve(&self.group, &tag, frame.v) {
      Ok(entry) => entry,
      Err(rejection) => return reply.exit(&rejection_outcome(&rejection)),
    };
    if !entry.codecs().supports(CodecId::Postcard) {
      return reply.exit(&rejected(StatusCode::UNSUPPORTED_MEDIA_TYPE, Some(&tag)));
    }
    reply.codecs = entry.codecs().clone();
    let kind = entry.kind();
    let streams_in = matches!(kind, ProcedureKind::ClientStream | ProcedureKind::Bidi);
    let (inbound_tx, inbound_rx) = if streams_in {
      let (tx, rx) = mpsc::unbounded_channel();
      (Some(tx), Some(rx))
    } else {
      (None, None)
    };
    let in_credits = Arc::new(AtomicU32::new(if streams_in { INITIAL_CREDITS } else { 0 }));
    let out_credits = Arc::new(Semaphore::new(INITIAL_CREDITS as usize));
    let start = CallStart {
      kind,
      tag: tag.clone(),
      version: frame.v,
      payload: frame.payload,
      frame_cursor: frame.cursor,
      origin: origin.clone(),
      deadline: envelope.deadline_ms.map(Duration::from_millis),
      inbound: inbound_rx,
      in_credits: Arc::clone(&in_credits),
      out_credits: Arc::clone(&out_credits),
      caps: (entry.max_items(), entry.max_bytes()),
    };
    {
      let mut calls = self.calls.lock().expect("calls");
      if calls.contains_key(&key) {
        return;
      }
      let from_peer = calls.values().filter(|c| c.origin == origin.name()).count();
      if calls.len() >= self.config.max_calls || from_peer >= self.config.max_calls_per_peer {
        drop(calls);
        return reply.exit(&CallOutcome::failure(exhausted("mesh.calls")));
      }
      calls.insert(
        key.clone(),
        ServerCall {
          generation: me.clone(),
          origin: origin.name().to_owned(),
          codecs: reply.codecs.clone(),
          inbound: inbound_tx,
          in_credits,
          out_credits,
          task: None,
          monitor: None,
        },
      );
    }
    // The caller's reply process (and its node) going down drops the call. Set up before the
    // task starts, so the entry is still there to hold it (unless the sink fired at once).
    let (weak_node, weak_self, watched) = (Arc::downgrade(node), Arc::downgrade(self), key.clone());
    let monitor = node.mesh.remote_monitor(
      &key.0,
      Box::new(move |_monitor, _pid, _reason| {
        if let (Some(node), Some(endpoint)) = (weak_node.upgrade(), weak_self.upgrade()) {
          endpoint.drop_call(&node, &watched);
        }
      }),
    );
    if let Some(call) = self.calls.lock().expect("calls").get_mut(&key) {
      call.monitor = Some(monitor);
    }
    if !self.calls.lock().expect("calls").contains_key(&key) {
      // The caller was already gone.
      return;
    }
    let trace = Some(envelope.trace)
      .filter(|trace| trace_bytes(trace) <= MAX_TRACE_BYTES)
      .unwrap_or_default();
    let method = tag
      .rsplit_once('.')
      .map_or(tag.as_str(), |(_, m)| m)
      .to_owned();
    let span = tracing::info_span!(
      "rpc.request",
      rpc.system = "id_effect_rpc",
      rpc.service = %self.service,
      rpc.method = %method,
      rpc.operation = %tag,
      mesh.node = %node.mesh.node_name(),
      mesh.from = %origin.name(),
      correlation.id = %cid,
    );
    let run = Arc::clone(self).run(reply, start).instrument(span);
    let task = match receiving_hook(node, &trace) {
      Some(hook) => {
        let mut attributes = vec![
          ("otel.kind".to_owned(), "server".to_owned()),
          ("rpc.system".to_owned(), "id_effect_rpc".to_owned()),
          ("rpc.service".to_owned(), self.service.clone()),
          ("rpc.method".to_owned(), method),
          ("rpc.procedure".to_owned(), tag),
          ("mesh.node".to_owned(), node.mesh.node_name().to_owned()),
          ("mesh.from".to_owned(), origin.name().to_owned()),
        ];
        if let Some(parent) = traceparent_in(&trace) {
          attributes.push(("mesh.parent".to_owned(), parent.to_owned()));
        }
        let traced: Effect<(), Never, ()> = from_async(move || async move {
          run.await;
          Ok::<(), Never>(())
        })
        .with_span_attrs("rpc.request", attributes)
        .with_span_hook(hook);
        node.runtime.spawn(async move {
          let _ = id_effect_axum::run_with_env((), move |_| traced).await;
        })
      }
      None => node.runtime.spawn(run),
    };
    if let Some(call) = self.calls.lock().expect("calls").get_mut(&key) {
      call.task = Some(task.abort_handle());
    }
  }

  fn inbound(self: &Arc<Self>, node: &Arc<Node>, key: &CallKey, item: Inbound) {
    let refused = {
      let calls = self.calls.lock().expect("calls");
      let Some(call) = calls.get(key) else {
        return;
      };
      let Some(tx) = &call.inbound else {
        return;
      };
      let within = matches!(item, Inbound::End) || take_one(&call.in_credits);
      if within {
        let _ = tx.send(item);
        false
      } else {
        true
      }
    };
    if refused {
      self.fail_call(node, key, exhausted("credits"));
    }
  }

  /// Remove `key`, abort its task and release its monitor.
  fn remove(&self, node: &Node, key: &CallKey) -> Option<ServerCall> {
    let call = self.calls.lock().expect("calls").remove(key)?;
    if let Some(task) = &call.task {
      task.abort();
    }
    if let Some(monitor) = call.monitor {
      node.mesh.remote_demonitor(&key.0, monitor);
    }
    Some(call)
  }

  fn drop_call(&self, node: &Arc<Node>, key: &CallKey) {
    self.remove(node, key);
  }

  fn fail_call(&self, node: &Arc<Node>, key: &CallKey, failure: RpcFailure) {
    if let Some(call) = self.remove(node, key) {
      Reply {
        node: Arc::clone(node),
        to: key.0.clone(),
        from: call.generation,
        id: key.1,
        codecs: call.codecs,
        cid: Arc::from("mesh"),
      }
      .exit(&CallOutcome::failure(failure));
    }
  }

  /// Drop every call the exited endpoint process `pid` ran.
  fn drop_generation(&self, node: &Arc<Node>, pid: &Pid) {
    let keys: Vec<CallKey> = self
      .calls
      .lock()
      .expect("calls")
      .iter()
      .filter(|(_, call)| &call.generation == pid)
      .map(|(key, _)| key.clone())
      .collect();
    for key in keys {
      self.remove(node, &key);
    }
  }

  /// Run one call to its exit.
  async fn run(self: Arc<Self>, reply: Reply, start: CallStart) {
    let outcome = self.outcome(&reply, start.clone_head(), start).await;
    if let Some(outcome) = outcome {
      reply.exit(&outcome);
    }
    // The call is over: forget it without aborting this task (it ends here).
    let call = self.calls.lock().expect("calls").remove(&reply_key(&reply));
    if let Some(monitor) = call.and_then(|call| call.monitor) {
      reply.node.mesh.remote_demonitor(&reply.to, monitor);
    }
  }

  /// The exit to send (`None`: a stream already sent it).
  async fn outcome(&self, reply: &Reply, head: CallHead, start: CallStart) -> Option<CallOutcome> {
    let entry = match dispatch::resolve(&self.group, &head.tag, head.version) {
      Ok(entry) => entry,
      Err(rejection) => return Some(rejection_outcome(&rejection)),
    };
    if let Err(rejection) = dispatch::authorize(entry, &peer_parts(&start.origin)).await {
      return Some(rejection_outcome(&rejection));
    }
    let ctx = CallContext::new(self.env.clone(), Arc::clone(&reply.cid));
    let mut control = CallControl::none();
    if matches!(
      start.kind,
      ProcedureKind::Query | ProcedureKind::Mutation | ProcedureKind::ClientStream
    ) {
      control = control.with_deadline(effective_deadline(start.deadline, self.config.max_deadline));
    }
    let window = INITIAL_CREDITS as usize;
    let input = || {
      decode_client_payload(&reply.codecs, start.payload.as_deref())
        .map_err(|e| CallOutcome::failure(invalid_input(e.to_string())))
    };
    let feed = |rx, tx, caps| feed(rx, tx, reply, Arc::clone(&start.in_credits), caps);
    let started = match start.kind {
      ProcedureKind::Query | ProcedureKind::Mutation => {
        let payload = match input() {
          Ok(payload) => payload,
          Err(outcome) => return Some(outcome),
        };
        dispatch::start_unary(entry, payload, ctx, control).map(Started::Unary)
      }
      ProcedureKind::ServerStream => {
        let payload = match input() {
          Ok(payload) => payload,
          Err(outcome) => return Some(outcome),
        };
        dispatch::start_server_stream(entry, reply.id, payload, ctx, control, window)
          .map(Started::Frames)
      }
      ProcedureKind::Subscription => {
        let payload = match input() {
          Ok(payload) => payload,
          Err(outcome) => return Some(outcome),
        };
        dispatch::start_subscription(
          entry,
          reply.id,
          payload,
          start.frame_cursor,
          ctx,
          control,
          window,
        )
        .map(Started::Frames)
      }
      ProcedureKind::ClientStream => dispatch::start_client_stream(entry, ctx, control, window)
        .map(|(tx, call)| Started::ClientStream(tx, call)),
      ProcedureKind::Bidi => dispatch::start_bidi(entry, reply.id, ctx, control, window)
        .map(|(tx, frames)| Started::Bidi(tx, frames)),
    };
    let rx = start.inbound;
    match started {
      Err(rejection) => Some(rejection_outcome(&rejection)),
      Ok(Started::Unary(call)) => Some(call.await),
      Ok(Started::Frames(frames)) => {
        reply.pump(frames, &start.out_credits).await;
        None
      }
      Ok(Started::ClientStream(tx, call)) => {
        let rx = rx?;
        let fed = Box::pin(feed(rx, tx, Some(start.caps)));
        match select(call, fed).await {
          Either::Left((outcome, _)) => Some(outcome),
          Either::Right((Ok(()), call)) => Some(call.await),
          Either::Right((Err(failure), _)) => Some(CallOutcome::failure(failure)),
        }
      }
      Ok(Started::Bidi(tx, frames)) => {
        let rx = rx?;
        let pump = Box::pin(reply.pump(frames, &start.out_credits));
        let fed = Box::pin(feed(rx, tx, None));
        match select(pump, fed).await {
          Either::Left(((), _)) => None,
          Either::Right((Ok(()), pump)) => {
            pump.await;
            None
          }
          Either::Right((Err(failure), _)) => Some(CallOutcome::failure(failure)),
        }
      }
    }
  }
}

fn reply_key(reply: &Reply) -> CallKey {
  (reply.to.clone(), reply.id)
}

/// The receiving span's hook for a call that carried `trace` (none when it carried none).
fn receiving_hook(node: &Node, trace: &TraceContext) -> Option<Arc<dyn id_effect::SpanHook>> {
  if trace.is_empty() {
    return None;
  }
  node.propagator().receiver_hook(trace)
}

struct CallHead {
  tag: String,
  version: Option<u32>,
}

impl CallStart {
  fn clone_head(&self) -> CallHead {
    CallHead {
      tag: self.tag.clone(),
      version: self.version,
    }
  }
}

enum Started {
  Unary(crate::handler::BoxCall),
  Frames(CallFrames),
  ClientStream(InboundSender, crate::handler::BoxCall),
  Bidi(InboundSender, CallFrames),
}

/// Feed a call's inbound items to its handler, granting a credit back per consumed item (in
/// batches); `caps` (items, bytes) bound a client stream.
async fn feed(
  mut rx: mpsc::UnboundedReceiver<Inbound>,
  tx: InboundSender,
  reply: &Reply,
  in_credits: Arc<AtomicU32>,
  caps: Option<(u64, u64)>,
) -> Result<(), RpcFailure> {
  let (mut items, mut bytes, mut consumed) = (0u64, 0u64, 0u32);
  while let Some(Inbound::Item(payload)) = rx.recv().await {
    items += 1;
    bytes += payload.len() as u64;
    if let Some((max_items, max_bytes)) = caps {
      if items > max_items {
        return Err(exhausted("items"));
      }
      if bytes > max_bytes {
        return Err(exhausted("bytes"));
      }
    }
    let value = decode_client_payload(&reply.codecs, Some(&payload))
      .map_err(|e| invalid_input(e.to_string()))?;
    if !tx.send(value).await {
      break;
    }
    consumed += 1;
    if consumed >= CREDIT_BATCH {
      in_credits.fetch_add(consumed, Ordering::SeqCst);
      reply.credit(consumed);
      consumed = 0;
    }
  }
  Ok(())
}

#[cfg(all(test, feature = "client"))]
mod tests {
  use super::*;
  use crate::client::mesh::MeshRpcError;
  use crate::codec::Codec as _;
  use crate::codec::postcard::Postcard;
  use crate::kinds::Duplex;
  use crate::{Procedure, WireSchema};
  use id_effect::{Cause, Exit, Stream, schema};
  use id_effect_process::{NodeName, ProcessNode};
  use std::time::Instant;

  fn int() -> WireSchema<i64> {
    WireSchema::new(schema::i64::<()>()).with_serde()
  }

  fn group() -> RpcGroup {
    let mut group = RpcGroup::new();
    group.query(
      Procedure::query("Pc.slow").public(),
      int(),
      int(),
      |n: i64, _: &mut DynEnv| {
        id_effect::sleep::<Never, DynEnv>(Duration::from_secs(30)).map(move |()| n)
      },
    );
    // Never reads its input, never ends.
    group.bidi(
      Procedure::bidi("Pc.deaf").public(),
      int(),
      int(),
      |_: &mut DynEnv| {
        succeed::<_, Never, _>(Duplex::new(|_items: Stream<i64, Never, DynEnv>| {
          Stream::from_iterable(0_i64..)
        }))
      },
    );
    group.bidi(
      Procedure::bidi("Pc.echo").public(),
      int(),
      int(),
      |_: &mut DynEnv| {
        succeed::<_, Never, _>(Duplex::new(|items: Stream<i64, Never, DynEnv>| items))
      },
    );
    group
  }

  struct Unit {
    id: NodeId,
    rpc: MeshRpc,
  }

  fn unit(name: &str) -> Unit {
    let mut registry = BehaviorRegistry::new();
    MeshRpc::register_behavior(&mut registry);
    let id = NodeId::from_parts(name, 7);
    let mesh = Mesh::new(id.clone(), ProcessNode::new(NodeName::new(name)), registry);
    Unit {
      rpc: MeshRpc::new(id.clone(), mesh).unwrap(),
      id,
    }
  }

  fn pair() -> (Unit, Unit) {
    let (a, b) = (unit("a@unit"), unit("b@unit"));
    a.rpc
      .mesh()
      .add_peer_node(&b.id, b.rpc.peer_sender(a.id.clone()));
    b.rpc
      .mesh()
      .add_peer_node(&a.id, a.rpc.peer_sender(b.id.clone()));
    (a, b)
  }

  fn id(n: u64) -> CallId {
    CallId::new(n).unwrap()
  }

  fn call(n: u64, tag: &str) -> Frame<Vec<u8>> {
    let mut frame = Frame::new(id(n), FrameKind::Call);
    frame.tag = Some(tag.to_owned());
    frame
  }

  fn item(n: u64, value: i64) -> Frame<Vec<u8>> {
    Frame::new(id(n), FrameKind::Item).with_payload(Postcard::encode(&value).unwrap())
  }

  /// A message for `node`'s RPC processes, stamped as from `origin`.
  fn stamped(node: &Node, origin: &NodeId, from: &Pid, frame: &Frame<Vec<u8>>) -> MeshEnvelope {
    MeshEnvelope {
      stamp: Some(Stamp {
        origin: origin.clone(),
        key: node.key,
      }),
      from: PidWire::from(from),
      trace: TraceContext::new(),
      deadline_ms: None,
      frame: encode_frame(frame).unwrap(),
    }
  }

  async fn eventually(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
      if done() {
        return true;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    done()
  }

  fn exited(node: &ProcessNode, pid: &Pid) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let set = Arc::clone(&flag);
    node.monitor_with_sink(
      pid,
      Box::new(move |_, _, _| set.store(true, Ordering::SeqCst)),
    );
    flag
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn the_ingress_stamps_casts_for_rpc_processes_only() {
    let b = unit("b@unit");
    let endpoint = b.rpc.serve("Pc", group(), DynEnv::new());
    let pid = endpoint.start().unwrap();
    let origin = NodeId::from_parts("a@unit", 1);
    let stamp = Stamp {
      origin: origin.clone(),
      key: [7; 32],
    };
    let shown = format!("{stamp:?}");
    assert!(shown.contains("a@unit") && !shown.contains("7, 7"));

    let to = PidWire::from(&pid);
    let other = PidWire::from(&Pid::from_parts(NodeName::new("b@unit"), 999_999, 0));
    let message = postcard::to_allocvec(&MeshEnvelope {
      stamp: None,
      from: PidWire::from(&pid),
      trace: TraceContext::new(),
      deadline_ms: None,
      frame: Vec::new(),
    })
    .unwrap();
    let node = &b.rpc.node;
    let stamp_of = |envelope: ProcessEnvelope| match envelope {
      ProcessEnvelope::Cast { msg, .. }
      | ProcessEnvelope::Reliable {
        message: ReliableMessage::Cast { msg, .. },
        ..
      } => postcard::from_bytes::<MeshEnvelope>(&msg)
        .ok()
        .and_then(|e| e.stamp),
      _ => None,
    };
    // A cast for an RPC process is stamped, also at least once.
    let cast = ProcessEnvelope::Cast {
      to: to.clone(),
      msg: message.clone(),
      trace: TraceContext::new(),
    };
    let stamp = stamp_of(node.stamp_envelope(&origin, cast)).unwrap();
    assert_eq!(stamp.origin, origin);
    assert!(same_key(&stamp.key, &node.key));
    let reliable = ProcessEnvelope::Reliable {
      seq: 1,
      floor: 0,
      message: ReliableMessage::Cast {
        to: to.clone(),
        msg: message.clone(),
        trace: TraceContext::new(),
      },
    };
    assert!(stamp_of(node.stamp_envelope(&origin, reliable)).is_some());
    // Other processes, other envelopes and undecodable bytes pass untouched.
    let elsewhere = ProcessEnvelope::Cast {
      to: other,
      msg: message.clone(),
      trace: TraceContext::new(),
    };
    assert_eq!(node.stamp_envelope(&origin, elsewhere.clone()), elsewhere);
    let info = ProcessEnvelope::Info {
      to: to.clone(),
      msg: message,
      trace: TraceContext::new(),
    };
    assert_eq!(node.stamp_envelope(&origin, info.clone()), info);
    let garbage = ProcessEnvelope::Cast {
      to,
      msg: vec![0xff; 3],
      trace: TraceContext::new(),
    };
    assert_eq!(node.stamp_envelope(&origin, garbage.clone()), garbage);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn the_endpoint_drops_what_it_cannot_trust_or_use() {
    let (a, b) = pair();
    let endpoint = b.rpc.serve("Pc", group(), DynEnv::new());
    let ep = endpoint.start().unwrap();
    let reply = a.rpc.node.reply_pid();
    let node = &b.rpc.node;
    let deliver = |envelope: MeshEnvelope| node.receive(&ep, envelope);

    // A verified call opens.
    deliver(stamped(node, &a.id, &reply, &call(1, "Pc.echo")));
    assert_eq!(endpoint.open_calls(), 1);
    // Unverified, wrongly keyed, naming another node, duplicate, undecodable, other kinds.
    let mut unverified = stamped(node, &a.id, &reply, &call(2, "Pc.echo"));
    unverified.stamp = None;
    deliver(unverified);
    let mut forged = stamped(node, &a.id, &reply, &call(2, "Pc.echo"));
    forged.stamp = Some(Stamp {
      origin: a.id.clone(),
      key: [0; 32],
    });
    deliver(forged);
    deliver(stamped(node, &b.id, &reply, &call(2, "Pc.echo")));
    deliver(stamped(node, &a.id, &reply, &call(1, "Pc.echo")));
    let mut garbage = stamped(node, &a.id, &reply, &call(2, "Pc.echo"));
    garbage.frame = vec![0xff; 4];
    deliver(garbage);
    deliver(stamped(
      node,
      &a.id,
      &reply,
      &Frame::new(id(1), FrameKind::Ping),
    ));
    deliver(stamped(node, &a.id, &reply, &item(99, 1)));
    assert_eq!(endpoint.open_calls(), 1);
    // An item that ends the input: the echo ends the call.
    deliver(stamped(
      node,
      &a.id,
      &reply,
      &item(1, 5).with_flags(FrameFlags::END),
    ));
    assert!(eventually(|| endpoint.open_calls() == 0).await);

    // Items for a call without input are ignored; a cancel drops it.
    deliver(stamped(
      node,
      &a.id,
      &reply,
      &call(3, "Pc.slow").with_payload(Postcard::encode(&1_i64).unwrap()),
    ));
    deliver(stamped(node, &a.id, &reply, &item(3, 1)));
    assert_eq!(endpoint.open_calls(), 1);
    deliver(stamped(
      node,
      &a.id,
      &reply,
      &Frame::new(id(3), FrameKind::Cancel),
    ));
    assert_eq!(endpoint.open_calls(), 0);

    // A caller already gone: its call is dropped before it runs.
    let gone = Pid::from_parts(NodeName::new("a@unit"), 999_999, 0);
    deliver(stamped(node, &a.id, &gone, &call(4, "Pc.echo")));
    assert_eq!(endpoint.open_calls(), 0);

    // Items beyond the credits fail the call.
    deliver(stamped(node, &a.id, &reply, &call(5, "Pc.deaf")));
    for n in 0..100 {
      deliver(stamped(node, &a.id, &reply, &item(5, n)));
    }
    assert!(eventually(|| endpoint.open_calls() == 0).await);

    // An oversized trace context is ignored, not refused.
    let mut traced = stamped(node, &a.id, &reply, &call(6, "Pc.echo"));
    traced.trace = vec![("traceparent".to_owned(), "x".repeat(MAX_TRACE_BYTES))];
    deliver(traced);
    assert_eq!(endpoint.open_calls(), 1);
    deliver(stamped(
      node,
      &a.id,
      &reply,
      &Frame::new(id(6), FrameKind::Cancel),
    ));
    assert_eq!(endpoint.open_calls(), 0);

    let wrong = rejection_outcome(&Rejection::WrongKind {
      tag: "Pc.echo".to_owned(),
      kind: ProcedureKind::Bidi,
    });
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn rpc_processes_need_their_node() {
    let b = unit("b@unit");
    let ep = b.rpc.serve("Pc", group(), DynEnv::new()).start().unwrap();
    let mesh = b.rpc.mesh().clone();
    // A call (not a cast) is answered with nothing.
    let unit_bytes = postcard::to_allocvec(&()).unwrap();
    let answered = mesh.remote_call::<()>(&ep, unit_bytes, Duration::from_secs(5));
    let exit = id_effect_axum::run_with_env((), move |_| answered).await;
    assert!(matches!(exit, Exit::Success(_)), "{exit:?}");

    // Spawned with an unknown host, the process does not start.
    let args = postcard::to_allocvec(&RpcProcessArgs { host: 0 }).unwrap();
    let orphan = mesh.spawn_local(BEHAVIOR, &args, None).unwrap();
    let orphan_down = exited(mesh.process(), &orphan);
    assert!(eventually(|| orphan_down.load(Ordering::SeqCst)).await);

    // Once the node's MeshRpc is gone, its processes stop on the next message.
    let reply = b.rpc.node.reply_pid();
    let reply_down = exited(mesh.process(), &reply);
    // Running (its `init` found the node) before the node goes.
    let unit_bytes = postcard::to_allocvec(&()).unwrap();
    let ready = mesh.remote_call::<()>(&reply, unit_bytes, Duration::from_secs(5));
    assert!(matches!(
      id_effect_axum::run_with_env((), move |_| ready).await,
      Exit::Success(_)
    ));
    drop(b);
    let message = postcard::to_allocvec(&MeshEnvelope {
      stamp: None,
      from: PidWire::from(&reply),
      trace: TraceContext::new(),
      deadline_ms: None,
      frame: Vec::new(),
    })
    .unwrap();
    mesh.remote_cast(&reply, message).unwrap();
    assert!(eventually(|| reply_down.load(Ordering::SeqCst)).await);
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_stale_local_holder_of_the_name_is_replaced() {
    let b = unit("b@unit");
    let stale = b
      .rpc
      .mesh()
      .process()
      .spawn::<RpcProcess>(
        RpcProcessArgs {
          host: b.rpc.node.host,
        },
        (),
      )
      .pid()
      .clone();
    b.rpc.mesh().global().register("rpc:Pc", &stale).unwrap();
    let started = b.rpc.serve("Pc", group(), DynEnv::new()).start().unwrap();
    assert_ne!(started, stale);
    assert_eq!(b.rpc.mesh().global().whereis("rpc:Pc"), Some(started));
  }

  async fn open_slot(node: &Node) -> (CallId, Pid) {
    assert!(eventually(|| !node.calls.lock().unwrap().is_empty()).await);
    let calls = node.calls.lock().unwrap();
    let (id, slot) = calls.iter().max_by_key(|(id, _)| **id).unwrap();
    (CallId::new(*id).unwrap(), slot.endpoint.clone())
  }

  async fn run<A: Send + 'static>(effect: Effect<A, MeshRpcError, ()>) -> Result<A, MeshRpcError> {
    match id_effect_axum::run_with_env((), move |_| effect).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("{cause:?}"),
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn clients_check_what_the_endpoint_sends() {
    let (a, b) = pair();
    b.rpc.serve("Pc", group(), DynEnv::new()).start().unwrap();
    assert!(eventually(|| a.rpc.mesh().global().whereis("rpc:Pc").is_some()).await);
    let pc = a.rpc.client("Pc");
    let node = Arc::clone(&a.rpc.node);
    let reply = node.reply_pid();

    // A unary call ignores frames it cannot trust and fails on frames it did not ask for.
    let slow = tokio::spawn(run(pc.query::<_, i64, ()>("Pc.slow", &1_i64)));
    let (call_id, ep) = open_slot(&node).await;
    let deliver = |frame: &Frame<Vec<u8>>, from: &Pid| {
      node.receive(&reply, stamped(&node, &b.id, from, frame));
    };
    let mut garbage = stamped(&node, &b.id, &ep, &Frame::new(call_id, FrameKind::End));
    garbage.frame = vec![0xff];
    node.receive(&reply, garbage);
    deliver(&Frame::new(id(call_id.get() + 1000), FrameKind::End), &ep);
    let impostor = Pid::from_parts(NodeName::new("b@unit"), 4242, 0);
    deliver(&Frame::new(call_id, FrameKind::End), &impostor);
    deliver(&Frame::new(call_id, FrameKind::End), &ep);
    let error = slow.await.unwrap().unwrap_err();
    assert_eq!(
      error,
      MeshRpcError::Transport("an unexpected frame".to_owned())
    );

    // Items beyond the credits (a unary call has none).
    let slow = tokio::spawn(run(pc.query::<_, i64, ()>("Pc.slow", &1_i64)));
    let (call_id, ep) = open_slot(&node).await;
    deliver(
      &Frame::new(call_id, FrameKind::Item).with_payload(vec![2]),
      &ep,
    );
    let error = slow.await.unwrap().unwrap_err();
    assert!(matches!(error, MeshRpcError::Transport(why) if why.contains("credits")));

    // The endpoint gone (no such process) while the call waits.
    let once = pc.clone().with_query_retries(0);
    let slow = tokio::spawn(run(once.query::<_, i64, ()>("Pc.slow", &1_i64)));
    let (call_id, _) = open_slot(&node).await;
    node.client_event(call_id.get(), ClientEvent::Down(ExitReason::NoProc));
    let error = slow.await.unwrap().unwrap_err();
    assert_eq!(error, MeshRpcError::NoEndpoint("Pc".to_owned()));

    // A stream ignores other kinds and ends on a violation.
    let (sink, mut out) = run(pc.bidi::<i64, i64, ()>("Pc.echo")).await.unwrap();
    let (call_id, ep) = open_slot(&node).await;
    deliver(&Frame::new(call_id, FrameKind::Pong), &ep);
    node.client_event(call_id.get(), ClientEvent::Violation("test"));
    assert_eq!(
      out.recv().await.unwrap().unwrap_err(),
      MeshRpcError::Transport("test".to_owned())
    );
    assert!(out.recv().await.is_none());
    drop((sink, out));

    // The reply process exiting fails the open calls; the next call starts a new one.
    let (_sink, mut out) = run(pc.bidi::<i64, i64, ()>("Pc.echo")).await.unwrap();
    a.rpc.mesh().process().exit(&reply, ExitReason::Killed);
    let error = out.recv().await.unwrap().unwrap_err();
    assert!(matches!(&error, MeshRpcError::EndpointDown(why) if why.contains("Killed")));
    let (sink, mut out) = run(pc.bidi::<i64, i64, ()>("Pc.echo")).await.unwrap();
    sink.send(&3).await.unwrap();
    assert_eq!(out.recv().await.unwrap().unwrap(), 3);
    assert_ne!(node.reply_pid(), reply);
  }
}
