//! **One QUIC endpoint, routed by ALPN** (feature `quic`; ADR 0014 D6/D7).
//!
//! A [`QuicEndpoint`] owns one UDP socket. Protocols register a [`Route`] for their ALPN —
//! [`ALPN_H3`] for HTTP/3, [`ALPN_MESH`] (`idm/1`) for the `id_effect_node` mesh — and receive
//! their connections from the [`AlpnRoute`] they get back. The [`AlpnRouter`] negotiates only
//! registered ALPNs (a client offering none of them fails the handshake), presents each route's
//! certificate, and hands each connection to its route.
//!
//! ## Client certificates (D7)
//!
//! rustls fixes the client-certificate verifier per server config, before the ALPN is known, so
//! "mutual TLS for `idm/1`, none for `h3`" cannot be a handshake rule on one endpoint. The
//! router derives the rule from the routes ([`ClientAuthMode`]):
//!
//! - **Only routes that require a verified client** (the mesh on its own port,
//!   `quic.mesh_port`): handshake-level mutual TLS ([`ClientAuthMode::Required`]).
//! - **Both kinds** (the shared shape): the server *requests* a client certificate as optional,
//!   with **no CA hints** ([`ClientAuthMode::Optional`]). A certificate that is sent is still
//!   verified against the route's trust during the handshake, and a bad one fails it. A route
//!   with [`ClientCertPolicy::RequireVerified`] then **refuses any connection without a verified
//!   client chain before its handler sees it** — the connection is closed with
//!   [`CLOSE_CLIENT_CERT_REQUIRED`] and no stream of it is ever read. Routes with
//!   [`ClientCertPolicy::Ignore`] (`h3`) accept clients with or without a certificate.
//! - **No route requires one**: no certificate is requested.
//!
//! One endpoint holds one client trust (the cluster CA or pins): a second route that requires a
//! verified client with its own trust is refused ([`QuicError::TrustConflict`]).
//!
//! ## Secure defaults (ADR 0013 R4, R8)
//!
//! - Address validation (QUIC Retry) is on: a client proves its address before the server
//!   spends a handshake on it. [`QuicEndpointConfig::dangerous_no_retry`] turns it off and is
//!   recorded in the posture registry.
//! - 0-RTT is off (the server never accepts early data; there is no opt-in).
//! - [`QuicLimits`] bound streams per connection, the idle timeout, the UDP payload, the
//!   receive windows, pending handshakes and the handshake time; datagrams are off. Raising any
//!   limit above its default needs [`QuicEndpointConfig::dangerous_raise_limits`] (recorded).
//!
//! ## Transport tuning (ADR 0014 D9)
//!
//! - **Congestion control per route** ([`CongestionControl`]): [`Route::h3`] runs BBR, the mesh
//!   ([`Route::mesh`], [`Route::new`]) runs CUBIC; [`Route::with_congestion_control`] changes
//!   either. On a shared endpoint a connection starts on CUBIC and switches to its route's
//!   algorithm as soon as the handshake has chosen the ALPN. quinn marks its BBR experimental.
//! - **UDP socket buffers**: the endpoint asks for 4 MiB each way ([`SocketBuffers`],
//!   [`QuicEndpointConfig::with_socket_buffers`]), best effort: the granted sizes are logged at
//!   `debug` ([`QuicEndpoint::socket_buffers`]); on Linux raise `net.core.rmem_max` and
//!   `net.core.wmem_max` to let the request through.
//!
//! ```rust
//! # #[tokio::main(flavor = "current_thread")] async fn main() {
//! use id_effect_platform::quic::{ClientAuthMode, QuicEndpoint, QuicEndpointConfig, Route};
//! use id_effect_platform::tls::{Identity, NodeCertificate};
//!
//! let cert = NodeCertificate::self_signed("localhost").unwrap();
//! let endpoint = QuicEndpoint::bind(
//!   "127.0.0.1:0".parse().unwrap(),
//!   QuicEndpointConfig::new(Identity::from_certificate(&cert).unwrap()),
//! )
//! .unwrap();
//! let _h3 = endpoint.register(Route::h3()).unwrap();
//! // The mesh pins its peers; sharing the port with h3 makes client certificates optional
//! // at the handshake and required by the idm/1 route.
//! let _mesh = endpoint.register(Route::mesh(None, vec![cert.cert_sha256()])).unwrap();
//! assert_eq!(endpoint.router().client_auth_mode(), ClientAuthMode::Optional);
//! # }
//! ```

mod config;
mod congestion;
mod endpoint;
mod router;
pub(crate) mod socket;

use crate::tls::CertError;
pub use config::{
  DANGEROUS_NO_RETRY, DANGEROUS_RAISE_LIMITS, DEFAULT_SOCKET_BUFFER, QuicEndpointConfig,
  QuicLimits, SocketBuffers, client_config, client_config_with, transport_config,
  transport_config_with,
};
// r12-e12-3-webtransport: QUIC datagrams on for endpoints that serve WebTransport.
pub use config::{DEFAULT_DATAGRAM_RECEIVE_BUFFER, MAX_DATAGRAM_RECEIVE_BUFFER};
pub use congestion::CongestionControl;
pub use endpoint::{EndpointStats, QuicEndpoint};
pub use router::{
  AlpnRoute, AlpnRouter, ClientAuthMode, ClientCertPolicy, Refusal, Route,
  has_verified_client_chain, negotiated_alpn,
};

/// ALPN of HTTP/3 (RFC 9114).
pub const ALPN_H3: &[u8] = b"h3";

/// ALPN of the `id_effect_node` mesh protocol. The mesh refuses connections without it, so a
/// 0.11 node (no ALPN) cannot join a 0.12 QUIC mesh.
pub const ALPN_MESH: &[u8] = b"idm/1";

/// Application close code: the route requires a verified client certificate and the
/// connection has none (ADR 0014 D7).
pub const CLOSE_CLIENT_CERT_REQUIRED: u32 = 0x1d7;

/// Application close code: the negotiated ALPN has no route that takes connections any more.
pub const CLOSE_NO_ROUTE: u32 = 0x1d8;

/// Application close code: the route's backlog is full.
pub const CLOSE_BUSY: u32 = 0x1d9;

/// A QUIC endpoint that cannot be built, bound or used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum QuicError {
  /// The UDP socket could not be bound (or no async runtime is running).
  #[error("quic: bind failed: {0}")]
  Bind(String),
  /// A limit or a protocol setting is unusable.
  #[error("quic: {0}")]
  Config(String),
  /// The TLS configuration could not be built.
  #[error(transparent)]
  Tls(#[from] CertError),
  /// The ALPN already has a route on this endpoint.
  #[error("quic: ALPN {0:?} is already routed")]
  AlpnTaken(String),
  /// A second route requires verified clients; one endpoint holds one client trust.
  #[error(
    "quic: ALPN {0:?} brings a second client-certificate trust; one endpoint holds one (give \
     the mesh its own port)"
  )]
  TrustConflict(String),
  /// A route requires verified clients but names no roots and no pins.
  #[error("quic: ALPN {0:?} requires a verified client but has no client trust (roots or pins)")]
  NoClientTrust(String),
  /// A limit is above its default without `dangerous_raise_limits`.
  #[error(
    "quic: limit {0} is above its default; raise it with QuicEndpointConfig::dangerous_raise_limits"
  )]
  LimitAboveDefault(&'static str),
  /// Dialing failed.
  #[error("quic connect: {0}")]
  Connect(String),
}

/// The ALPN as text, for messages.
fn alpn_text(alpn: &[u8]) -> String {
  String::from_utf8_lossy(alpn).into_owned()
}
