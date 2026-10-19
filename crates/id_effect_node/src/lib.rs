//! # id_effect_node — distribution layer of the Process Mesh
//!
//! Node identity, cookie-authenticated handshake, pluggable transports,
//! SWIM membership, remote processes (at-most-once by default, at-least-once
//! on request: [`delivery`]), process groups, and distributed supervision
//! (ADR 0009).

#![deny(missing_docs)]

pub mod control;
pub mod delivery;
pub mod discovery;
pub mod dist_supervisor;
pub mod envelope;
pub mod fanout;
pub mod global_registry;
pub mod groups;
pub mod handshake;
pub mod id;
pub mod membership;
pub mod mesh;
pub mod placement;
pub mod registry;
pub mod session;
pub mod singleton;
pub mod transport;
pub mod wire;

pub use control::{
  ClusterStatus, DrainRequest, MemberView, ProcessView, RebalanceRequest, TreeNodeView,
};
pub use delivery::{
  DeliveryConfig, DeliveryFailed, DeliveryFailure, DeliveryMode, DeliveryMonitor, SendReliable,
};
pub use discovery::{ChainedDiscovery, Discovery, DnsDiscovery, StaticDiscovery};
#[cfg(feature = "kubernetes")]
pub use discovery::{KubernetesDiscovery, KubernetesError};
pub use dist_supervisor::{DistChildSpec, DistributedSupervisor};
pub use envelope::{
  ExitReasonWire, PidWire, ProcessEnvelope, ReliableMessage, RemoteFault, TRACEPARENT,
  TraceContext, TracePropagator, W3cTraceContext, decode_envelope, empty_trace, encode_envelope,
  trace_pair, traceparent_in, traceparent_of,
};
pub use fanout::{SharedGroups, fanout_cast, fanout_cast_on_node};
pub use global_registry::{
  GlobalNameError, GlobalRegistry, MAX_NAME_LEN, NameClaim, conflicting_name, name_conflict,
};
pub use groups::{Dot, OrSwot, ProcessGroups};
pub use handshake::{Cookie, CookieError, HandshakeError};
pub use id::NodeId;
pub use membership::{
  Member, Membership, MembershipError, MembershipEvent, NodeMonitors, Output, TimerWheel,
};
pub use mesh::{Mesh, MeshFabric, PeerSender};
pub use placement::{
  ClusterResourcePolicy, ClusterTelemetry, LoadReport, NodeCandidate, PlacementMode,
  pick_placement, place, scale_out_placed, score_candidate,
};
pub use registry::{BehaviorEntry, BehaviorRegistry};
pub use session::{Session, SessionError};
#[cfg(feature = "pg")]
pub use singleton::PgAdvisoryLease;
pub use singleton::{
  ClusterSingleton, MemoryLease, SingletonError, SingletonLease, advisory_lock_keys,
};
pub use transport::memory::{InMemoryTransport, MemoryNetwork};
pub use transport::tcp::TcpTransport;
#[cfg(feature = "tls-tcp")]
pub use transport::tls_tcp::{TlsTcpConfig, TlsTcpConfigError};
pub use transport::{Connection, Listener, Transport, TransportError};
pub use wire::{Frame, PROTOCOL_VERSION, WireError};

/// Compiles and runs the README's quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
