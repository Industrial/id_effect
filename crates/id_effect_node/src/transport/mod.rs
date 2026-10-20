//! Transport abstraction: how raw frames move between nodes.
//!
//! Three implementations per ADR 0009:
//! - [`memory::InMemoryTransport`] — first-class, deterministic, supports
//!   scripted partitions (the chaos-test backbone).
//! - [`tcp`] — length-delimited frames over Tokio TCP: `TcpTransport::tls`
//!   (feature `tls-tcp`, TLS 1.3 with mutual auth; configured in `tls_tcp`).
//! - `quic` (feature `quic`) — quinn + rustls with mutual TLS (the
//!   production path).
//!
//! **Encrypted by default** (dist.tls#2, ADR 0013 R8): every constructor of
//! a network transport is mutual TLS, and a transport without a way to
//! verify its peers refuses to dial or listen. The only plaintext network
//! path is `TcpTransport::dangerous_plaintext`, which exists only with the
//! feature `insecure-plaintext-tcp` and records the opt-out in the posture
//! registry ([`id_effect::observability::posture`]).
//!
//! Certificates for the encrypted transports (PEM loading, CA bundles,
//! reload on rotation) live in `certs` (features `quic`, `tls-tcp`).
//!
//! A transport moves **bytes**; framing, handshake, and semantics live
//! above it. Connections are bidirectional byte-frame pipes.

use bytes::Bytes;

pub mod memory;
pub mod tcp;

#[cfg(any(feature = "quic", feature = "tls-tcp"))]
pub mod certs;
#[cfg(feature = "quic")]
pub mod quic;
#[cfg(feature = "tls-tcp")]
pub mod tls_tcp;

/// Transport-level failure.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
  /// The peer is unreachable or refused the connection.
  #[error("connect failed: {0}")]
  Connect(String),
  /// The connection was lost (peer closed, network dropped, partition).
  #[error("connection closed")]
  Closed,
  /// Listener/bind failure.
  #[error("bind failed: {0}")]
  Bind(String),
  /// I/O error underneath.
  #[error("io: {0}")]
  Io(String),
}

impl From<std::io::Error> for TransportError {
  fn from(err: std::io::Error) -> Self {
    TransportError::Io(err.to_string())
  }
}

/// A bidirectional, ordered, frame-oriented connection to one peer.
///
/// Guarantees (normative): frames are delivered in order per direction or
/// the connection reports [`TransportError::Closed`]; no partial frames.
// async-trait emits `#[must_use]` on methods returning an already-must_use boxed future.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Connection: Send + Sync {
  /// Send one frame. Errors mean the connection is dead.
  async fn send(&self, frame: Bytes) -> Result<(), TransportError>;
  /// Receive the next frame. Errors mean the connection is dead.
  async fn recv(&self) -> Result<Bytes, TransportError>;
  /// Close the connection (idempotent).
  async fn close(&self);
  /// Address of the remote end, for diagnostics.
  fn peer_addr(&self) -> String;
  /// Cap the size of frames `recv` accepts, in bytes. Transports that read
  /// length-prefixed frames off the network enforce it before allocating;
  /// the handshake lowers it to
  /// [`PRE_AUTH_MAX_FRAME_BYTES`](crate::handshake::PRE_AUTH_MAX_FRAME_BYTES)
  /// until the peer has authenticated. The default ignores it (in-process
  /// transports never allocate on a peer's behalf).
  fn set_max_frame_bytes(&self, max: u32) {
    let _ = max;
  }
  /// A 32-byte secret only the two ends of this connection share: the TLS
  /// exporter for [`CHANNEL_BINDING_LABEL`](crate::handshake::CHANNEL_BINDING_LABEL).
  /// The handshake mixes it into the cookie proof so a proof cannot be
  /// relayed onto another channel. `None` (the default) for transports
  /// without a secure channel.
  fn channel_binding(&self) -> Option<[u8; 32]> {
    None
  }
}

/// Accepts inbound connections on a bound address.
#[allow(clippy::double_must_use)] // see `Connection`
#[async_trait::async_trait]
pub trait Listener: Send + Sync {
  /// Wait for the next inbound connection.
  async fn accept(&self) -> Result<Box<dyn Connection>, TransportError>;
  /// The address this listener is bound to (resolves ephemeral ports).
  fn local_addr(&self) -> String;
}

/// A way of dialing and listening — the only thing the distribution layer
/// needs from the network.
#[allow(clippy::double_must_use)] // see `Connection`
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
  /// Dial a peer address.
  async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>, TransportError>;
  /// Bind a listener.
  async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>, TransportError>;
}
