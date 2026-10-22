//! [`SessionPump`] — runs one authenticated [`Session`] for a [`Mesh`].
//!
//! Started on a session, the pump makes the peer reachable ([`Mesh::add_peer_node`]), writes
//! what the mesh sends it, reads every envelope the peer sends, and reports the peer down
//! ([`Mesh::peer_down`]) when the session ends or the pump is dropped — which fires the monitors
//! on the peer's processes.
//!
//! ## Verified ingress (ADR 0013 R1, R8)
//!
//! The handshake proves which node is on the other end ([`Session::peer`]). Before anything
//! acts on an envelope, the pump checks that it is consistent with that identity
//! ([`verify_envelope`]): messages for processes are addressed to this node, and whatever speaks
//! for a process or a name — a link, an exit signal, a monitor's answer, a spawn reply, a
//! cluster-wide name claim — speaks for one on the peer. An envelope that fails is dropped and
//! counted ([`SessionPumpStats::unverified`]); one that passes goes to the pump's [`Ingress`].
//!
//! The ingress is the node's verified receiving path: the [`Mesh`] itself, or a layer that must
//! see every inbound envelope first (`id_effect_rpc`'s `MeshRpc` stamps the messages for its
//! RPC processes with the verified node before the mesh dispatches them). The pump never hands an
//! envelope to [`Mesh::handle_envelope_from`] around its ingress, so a layer installed as the
//! ingress sees everything the session carries.
//!
//! ```rust
//! use id_effect_node::session_pump::verify_envelope;
//! use id_effect_node::{NodeId, PidWire, ProcessEnvelope};
//!
//! let peer = NodeId::from_parts("b@host", 1);
//! let cast = |node: &str| ProcessEnvelope::Cast {
//!   to: PidWire { node: node.into(), id: 1, incarnation: 1 },
//!   msg: vec![],
//!   trace: vec![],
//! };
//! assert!(verify_envelope("a@host", &peer, &cast("a@host")).is_ok());
//! assert!(verify_envelope("a@host", &peer, &cast("c@host")).is_err());
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::envelope::{PidWire, ProcessEnvelope, RemoteFault, decode_envelope};
use crate::id::NodeId;
use crate::mesh::{Mesh, PeerSender};
use crate::session::Session;
use crate::wire::Frame;

/// Default number of envelopes queued for the peer before the pump's sender refuses more.
pub const DEFAULT_SEND_QUEUE: usize = 4_096;

/// The receiving side of a node: where a [`SessionPump`] hands the envelopes it verified.
pub trait Ingress: Send + Sync + 'static {
  /// The mesh the peer joins.
  fn mesh(&self) -> &Mesh;

  /// Deliver `envelope`, which the authenticated session with `origin` carried and the pump
  /// verified.
  ///
  /// # Errors
  ///
  /// Whatever the mesh's dispatch answers (a missing process, an undecodable payload, …); the
  /// pump counts it ([`SessionPumpStats::refused`]) and goes on.
  fn deliver(&self, origin: &NodeId, envelope: ProcessEnvelope) -> Result<(), RemoteFault>;
}

/// A plain mesh is its own ingress: verified envelopes go to its dispatch, which routes replies
/// back to `origin` and keys what it keeps for the peer by its full identity.
impl Ingress for Mesh {
  fn mesh(&self) -> &Mesh {
    self
  }

  fn deliver(&self, origin: &NodeId, envelope: ProcessEnvelope) -> Result<(), RemoteFault> {
    self.handle_envelope_from(origin, envelope)
  }
}

/// Why [`verify_envelope`] refused an envelope.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unverified {
  /// A message for a process of another node (`to`/`target` names that node).
  #[error("addressed to a process of {0}, not of this node")]
  NotForThisNode(String),
  /// The envelope speaks for a process or a name of a node other than the authenticated peer.
  #[error("speaks for {0}, not for the authenticated peer")]
  ForeignOrigin(String),
}

/// Check that `envelope`, carried by the session authenticated as `peer`, is consistent with it
/// on the node named `local`:
///
/// - casts, bare messages, calls and at-least-once messages target a process of `local`, and so
///   do monitor and demonitor requests;
/// - links, unlinks and exit signals come from a process of `peer` to one of `local`;
/// - a `Down` names a process of `peer`, a successful spawn reply a pid of `peer`, a spawn's
///   `link_to` a process of `peer`;
/// - name claims, releases and syncs are about names `peer` owns.
///
/// Replies and acknowledgements carry no identity and always pass.
///
/// # Errors
///
/// [`Unverified`] naming the node the envelope wrongly addresses or speaks for.
pub fn verify_envelope(
  local: &str,
  peer: &NodeId,
  envelope: &ProcessEnvelope,
) -> Result<(), Unverified> {
  let to_local = |pid: &PidWire| {
    if pid.node == local {
      Ok(())
    } else {
      Err(Unverified::NotForThisNode(pid.node.clone()))
    }
  };
  let of_peer = |node: &str| {
    if node == peer.name() {
      Ok(())
    } else {
      Err(Unverified::ForeignOrigin(node.to_string()))
    }
  };
  match envelope {
    ProcessEnvelope::Cast { to, .. }
    | ProcessEnvelope::Info { to, .. }
    | ProcessEnvelope::Call { to, .. } => to_local(to),
    ProcessEnvelope::Reliable { message, .. } => to_local(message.to()),
    ProcessEnvelope::Monitor { target, .. } | ProcessEnvelope::Demonitor { target, .. } => {
      to_local(target)
    }
    ProcessEnvelope::Down { target, .. } => of_peer(&target.node),
    ProcessEnvelope::Link { from, to }
    | ProcessEnvelope::Unlink { from, to }
    | ProcessEnvelope::ExitSignal { from, to, .. } => {
      of_peer(&from.node)?;
      to_local(to)
    }
    ProcessEnvelope::Spawn { link_to, .. } => {
      link_to.as_ref().map_or(Ok(()), |pid| of_peer(&pid.node))
    }
    ProcessEnvelope::SpawnReply { result, .. } => match result {
      Ok(pid) => of_peer(&pid.node),
      Err(_) => Ok(()),
    },
    ProcessEnvelope::GlobalRegister { claim } => {
      of_peer(claim.owner.name())?;
      of_peer(&claim.pid.node)
    }
    ProcessEnvelope::GlobalUnregister { pid, .. } => of_peer(&pid.node),
    ProcessEnvelope::GlobalSync { claims } => claims.iter().try_for_each(|claim| {
      of_peer(claim.owner.name())?;
      of_peer(&claim.pid.node)
    }),
    ProcessEnvelope::Reply { .. } | ProcessEnvelope::Ack { .. } => Ok(()),
  }
}

/// Settings of a [`SessionPump`] ([`SessionPump::start_with`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPumpConfig {
  send_queue: usize,
}

impl Default for SessionPumpConfig {
  fn default() -> Self {
    Self {
      send_queue: DEFAULT_SEND_QUEUE,
    }
  }
}

impl SessionPumpConfig {
  /// The defaults: [`DEFAULT_SEND_QUEUE`].
  pub fn new() -> Self {
    Self::default()
  }

  /// Queue at most `envelopes` (≥ 1) for the peer; beyond that the mesh's sends to it fail with
  /// [`RemoteFault::NoProc`] until the writer catches up.
  #[must_use]
  pub fn with_send_queue(mut self, envelopes: usize) -> Self {
    self.send_queue = envelopes.max(1);
    self
  }

  /// The send queue's bound.
  pub fn send_queue(&self) -> usize {
    self.send_queue
  }
}

/// What a [`SessionPump`] did with the envelopes the peer sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionPumpStats {
  /// Verified and handed to the ingress, which accepted them.
  pub delivered: u64,
  /// Verified and handed to the ingress, which answered a fault (for instance a cast to a
  /// process that is gone).
  pub refused: u64,
  /// Dropped by [`verify_envelope`]: never reached the ingress.
  pub unverified: u64,
}

#[derive(Default)]
struct Counters {
  delivered: AtomicU64,
  refused: AtomicU64,
  unverified: AtomicU64,
}

impl Counters {
  fn snapshot(&self) -> SessionPumpStats {
    SessionPumpStats {
      delivered: self.delivered.load(Ordering::Relaxed),
      refused: self.refused.load(Ordering::Relaxed),
      unverified: self.unverified.load(Ordering::Relaxed),
    }
  }
}

/// Verify one envelope from `peer` and hand it to `ingress`, counting the outcome.
fn admit(ingress: &dyn Ingress, peer: &NodeId, envelope: ProcessEnvelope, counters: &Counters) {
  let local = ingress.mesh().node_name();
  if let Err(why) = verify_envelope(local, peer, &envelope) {
    counters.unverified.fetch_add(1, Ordering::Relaxed);
    tracing::warn!(
      target: "id_effect_node::session_pump",
      node = %local,
      peer = %peer.name(),
      reason = %why,
      "dropped an unverified envelope"
    );
    return;
  }
  match ingress.deliver(peer, envelope) {
    Ok(()) => counters.delivered.fetch_add(1, Ordering::Relaxed),
    Err(_) => counters.refused.fetch_add(1, Ordering::Relaxed),
  };
}

/// One live session between a node's [`Mesh`] and a peer, pumped in both directions on the
/// current Tokio runtime. Dropping it closes the session and reports the peer down.
pub struct SessionPump {
  mesh: Mesh,
  peer: NodeId,
  session: Arc<Session>,
  counters: Arc<Counters>,
  tasks: Vec<JoinHandle<()>>,
}

impl fmt::Debug for SessionPump {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SessionPump")
      .field("node", &self.mesh.node_name())
      .field("peer", &self.peer.name())
      .field("stats", &self.stats())
      .finish_non_exhaustive()
  }
}

impl SessionPump {
  /// Pump `session` into `ingress` with the default [`SessionPumpConfig`].
  ///
  /// # Panics
  ///
  /// Outside a Tokio runtime.
  pub fn start(ingress: impl Ingress, session: Session) -> Self {
    Self::start_with(ingress, session, SessionPumpConfig::default())
  }

  /// Pump `session` into `ingress`: register the peer with the ingress's mesh (its sends to the
  /// peer go through a queue of `config.send_queue()` envelopes), and verify every envelope the
  /// peer sends before the ingress sees it. An undecodable envelope, a `Close` frame or a
  /// transport failure ends the session; liveness frames (`Ping`, `Pong`) are ignored.
  ///
  /// # Panics
  ///
  /// Outside a Tokio runtime.
  pub fn start_with(ingress: impl Ingress, session: Session, config: SessionPumpConfig) -> Self {
    let ingress: Arc<dyn Ingress> = Arc::new(ingress);
    let mesh = ingress.mesh().clone();
    let session = Arc::new(session);
    let peer = session.peer().clone();
    let counters = Arc::new(Counters::default());
    let (tx, mut rx) = mpsc::channel::<ProcessEnvelope>(config.send_queue);
    let sender: PeerSender =
      Arc::new(move |envelope| tx.try_send(envelope).map_err(|_| RemoteFault::NoProc));
    mesh.add_peer_node(&peer, sender);
    let writer = {
      let session = Arc::clone(&session);
      tokio::spawn(async move {
        while let Some(envelope) = rx.recv().await {
          if Mesh::session_send(&session, &envelope).await.is_err() {
            break;
          }
        }
        session.close().await;
      })
    };
    let reader = {
      let (session, peer, counters) = (Arc::clone(&session), peer.clone(), Arc::clone(&counters));
      tokio::spawn(async move {
        loop {
          match session.recv().await {
            Ok(Frame::Data(bytes)) => match decode_envelope(&bytes) {
              Ok(envelope) => admit(ingress.as_ref(), &peer, envelope, &counters),
              Err(e) => {
                tracing::warn!(
                  target: "id_effect_node::session_pump",
                  peer = %peer.name(),
                  error = %e,
                  "undecodable envelope: closing the session"
                );
                break;
              }
            },
            Ok(Frame::Close(_)) | Err(_) => break,
            Ok(_) => {}
          }
        }
        // Forgetting the peer drops its sender, so the writer drains and closes the session.
        ingress.mesh().peer_down(peer.name());
      })
    };
    Self {
      mesh,
      peer,
      session,
      counters,
      tasks: vec![writer, reader],
    }
  }

  /// This side's node name.
  pub fn node(&self) -> &str {
    self.mesh.node_name()
  }

  /// The authenticated peer.
  pub fn peer(&self) -> &NodeId {
    &self.peer
  }

  /// Whether the session still runs (neither side closed it).
  pub fn is_open(&self) -> bool {
    self.tasks.iter().all(|task| !task.is_finished())
  }

  /// What the pump did with the envelopes received so far.
  pub fn stats(&self) -> SessionPumpStats {
    self.counters.snapshot()
  }
}

impl Drop for SessionPump {
  fn drop(&mut self) {
    for task in &self.tasks {
      task.abort();
    }
    self.mesh.peer_down(self.peer.name());
    // Tell the peer: close the connection on the runtime (a drop cannot wait for it).
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      let session = Arc::clone(&self.session);
      drop(runtime.spawn(async move { session.close().await }));
    }
  }
}

#[cfg(test)]
mod tests {
  use id_effect_process::{NodeName, ProcessNode};

  use super::*;
  use crate::envelope::{ExitReasonWire, ReliableMessage};
  use crate::global_registry::NameClaim;
  use crate::registry::BehaviorRegistry;

  fn pid(node: &str) -> PidWire {
    PidWire {
      node: node.to_string(),
      id: 1,
      incarnation: 1,
    }
  }

  fn claim(owner: &str, pid_node: &str) -> NameClaim {
    NameClaim {
      name: "svc".to_string(),
      pid: pid(pid_node),
      owner: NodeId::from_parts(owner, 1),
    }
  }

  fn check(envelope: &ProcessEnvelope) -> Result<(), Unverified> {
    verify_envelope("a@t", &NodeId::from_parts("b@t", 9), envelope)
  }

  #[test]
  fn envelopes_consistent_with_the_peer_pass() {
    let passing = [
      ProcessEnvelope::Cast {
        to: pid("a@t"),
        msg: vec![],
        trace: vec![],
      },
      ProcessEnvelope::Info {
        to: pid("a@t"),
        msg: vec![],
        trace: vec![],
      },
      ProcessEnvelope::Call {
        to: pid("a@t"),
        call_id: 1,
        msg: vec![],
        trace: vec![],
      },
      ProcessEnvelope::Reliable {
        seq: 1,
        floor: 1,
        message: ReliableMessage::Info {
          to: pid("a@t"),
          msg: vec![],
          trace: vec![],
        },
      },
      ProcessEnvelope::Monitor {
        monitor: 1,
        target: pid("a@t"),
      },
      ProcessEnvelope::Demonitor {
        monitor: 1,
        target: pid("a@t"),
      },
      ProcessEnvelope::Down {
        monitor: 1,
        target: pid("b@t"),
        reason: ExitReasonWire::Normal,
      },
      ProcessEnvelope::Link {
        from: pid("b@t"),
        to: pid("a@t"),
      },
      ProcessEnvelope::Unlink {
        from: pid("b@t"),
        to: pid("a@t"),
      },
      ProcessEnvelope::ExitSignal {
        from: pid("b@t"),
        to: pid("a@t"),
        reason: ExitReasonWire::Killed,
      },
      ProcessEnvelope::Spawn {
        spawn_id: 1,
        behavior: "x".into(),
        args: vec![],
        link_to: None,
      },
      ProcessEnvelope::Spawn {
        spawn_id: 1,
        behavior: "x".into(),
        args: vec![],
        link_to: Some(pid("b@t")),
      },
      ProcessEnvelope::SpawnReply {
        spawn_id: 1,
        result: Ok(pid("b@t")),
      },
      ProcessEnvelope::SpawnReply {
        spawn_id: 1,
        result: Err(RemoteFault::NoProc),
      },
      ProcessEnvelope::GlobalRegister {
        claim: claim("b@t", "b@t"),
      },
      ProcessEnvelope::GlobalUnregister {
        name: "svc".into(),
        pid: pid("b@t"),
      },
      ProcessEnvelope::GlobalSync {
        claims: vec![claim("b@t", "b@t")],
      },
      ProcessEnvelope::Reply {
        call_id: 1,
        result: Ok(vec![]),
      },
      ProcessEnvelope::Ack { upto: 1, seq: 1 },
    ];
    for envelope in &passing {
      assert_eq!(check(envelope), Ok(()), "{envelope:?}");
    }
  }

  #[test]
  fn envelopes_inconsistent_with_the_peer_fail() {
    let elsewhere = || Unverified::NotForThisNode("c@t".into());
    let foreign = || Unverified::ForeignOrigin("c@t".into());
    let failing = [
      (
        ProcessEnvelope::Cast {
          to: pid("c@t"),
          msg: vec![],
          trace: vec![],
        },
        elsewhere(),
      ),
      (
        ProcessEnvelope::Reliable {
          seq: 1,
          floor: 1,
          message: ReliableMessage::Cast {
            to: pid("c@t"),
            msg: vec![],
            trace: vec![],
          },
        },
        elsewhere(),
      ),
      (
        ProcessEnvelope::Monitor {
          monitor: 1,
          target: pid("c@t"),
        },
        elsewhere(),
      ),
      (
        ProcessEnvelope::Down {
          monitor: 1,
          target: pid("c@t"),
          reason: ExitReasonWire::Normal,
        },
        foreign(),
      ),
      (
        ProcessEnvelope::ExitSignal {
          from: pid("c@t"),
          to: pid("a@t"),
          reason: ExitReasonWire::Killed,
        },
        foreign(),
      ),
      (
        ProcessEnvelope::Link {
          from: pid("b@t"),
          to: pid("c@t"),
        },
        elsewhere(),
      ),
      (
        ProcessEnvelope::Spawn {
          spawn_id: 1,
          behavior: "x".into(),
          args: vec![],
          link_to: Some(pid("c@t")),
        },
        foreign(),
      ),
      (
        ProcessEnvelope::SpawnReply {
          spawn_id: 1,
          result: Ok(pid("c@t")),
        },
        foreign(),
      ),
      (
        ProcessEnvelope::GlobalRegister {
          claim: claim("c@t", "c@t"),
        },
        foreign(),
      ),
      (
        ProcessEnvelope::GlobalRegister {
          claim: claim("b@t", "c@t"),
        },
        foreign(),
      ),
      (
        ProcessEnvelope::GlobalUnregister {
          name: "svc".into(),
          pid: pid("c@t"),
        },
        foreign(),
      ),
      (
        ProcessEnvelope::GlobalSync {
          claims: vec![claim("b@t", "b@t"), claim("c@t", "c@t")],
        },
        foreign(),
      ),
      (
        ProcessEnvelope::GlobalSync {
          claims: vec![claim("b@t", "c@t")],
        },
        foreign(),
      ),
    ];
    for (envelope, why) in failing {
      assert_eq!(check(&envelope), Err(why), "{envelope:?}");
    }
    assert!(foreign().to_string().contains("c@t"));
    assert!(elsewhere().to_string().contains("c@t"));
  }

  #[test]
  fn admit_counts_every_outcome() {
    let node = NodeName::new("a@t");
    let mesh = Mesh::new(
      NodeId::fresh(&node),
      ProcessNode::new(node),
      BehaviorRegistry::new(),
    );
    let peer = NodeId::from_parts("b@t", 1);
    let counters = Counters::default();
    admit(
      &mesh,
      &peer,
      ProcessEnvelope::Ack { upto: 0, seq: 0 },
      &counters,
    );
    admit(
      &mesh,
      &peer,
      ProcessEnvelope::Cast {
        to: pid("a@t"),
        msg: vec![],
        trace: vec![],
      },
      &counters,
    );
    admit(
      &mesh,
      &peer,
      ProcessEnvelope::Cast {
        to: pid("c@t"),
        msg: vec![],
        trace: vec![],
      },
      &counters,
    );
    assert_eq!(
      counters.snapshot(),
      SessionPumpStats {
        delivered: 1,
        refused: 1,
        unverified: 1,
      }
    );
  }

  #[test]
  fn a_pump_dropped_off_its_runtime_still_reports_the_peer_down() {
    use crate::handshake::Cookie;
    use crate::transport::Transport as _;
    use crate::transport::memory::MemoryNetwork;

    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("runtime");
    let node = NodeName::new("b@t");
    let mesh = Mesh::new(
      NodeId::fresh(&node),
      ProcessNode::new(node),
      BehaviorRegistry::new(),
    );
    let (pump, _dialer) = runtime.block_on(async {
      let net = MemoryNetwork::new();
      let listener = net.transport("b@t").listen("b@t").await.expect("bind");
      let dial = net.transport("a@t").connect("b@t").await.expect("dial");
      let accepted = listener.accept().await.expect("accept");
      let cookie = Cookie::new("pump-unit-cookie-of-at-least-32-bytes");
      let dialer = NodeId::from_parts("a@t", 1);
      let (a, b) = tokio::join!(
        Session::initiate(dial, &dialer, &cookie),
        Session::respond(accepted, mesh.node_id(), &cookie),
      );
      (
        SessionPump::start(mesh.clone(), b.expect("b")),
        a.expect("a"),
      )
    });
    drop(runtime);
    let remote = id_effect_process::Pid::from_parts(NodeName::new("a@t"), 1, 1);
    drop(pump);
    assert!(mesh.remote_cast(&remote, Vec::new()).is_err(), "a is down");
  }

  #[test]
  fn config_bounds_the_queue() {
    assert_eq!(SessionPumpConfig::new().send_queue(), DEFAULT_SEND_QUEUE);
    assert_eq!(SessionPumpConfig::new().with_send_queue(0).send_queue(), 1);
  }
}
