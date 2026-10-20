//! [`QuicTransport`] — quinn + rustls (feature `quic`).
//!
//! Each logical connection is one QUIC connection with a single
//! bidirectional control stream carrying length-delimited frames (the
//! same layout as the TCP transport). Multiplexed per-process-pair
//! streams are a wire-compatible upgrade reserved for a later phase.
//!
//! ## Endpoint and ALPN
//!
//! The mesh speaks ALPN [`ALPN_MESH`] (`idm/1`) and runs on an
//! [`id_effect_platform::quic::QuicEndpoint`] (address validation on, 0-RTT
//! off, finite stream, window and idle limits). A peer that offers no or
//! another ALPN fails the handshake, so a 0.11 node cannot join. Two shapes:
//!
//! - **Own endpoint** ([`QuicTransport::new`] / [`QuicTransport::try_new`]):
//!   [`Transport::listen`] binds an endpoint that serves only `idm/1`, with
//!   handshake-level mutual TLS (the 0.11 behaviour; `quic.mesh_port`).
//! - **Shared endpoint** ([`QuicTransport::on_endpoint`]): the mesh registers
//!   `idm/1` on an endpoint that also serves HTTP/3 on the same UDP port.
//!   The handshake then requests client certificates as optional (no CA
//!   hints) and still verifies one that is sent; the `idm/1` route closes a
//!   connection without a verified client chain before any frame of it is
//!   read (ADR 0014 D7). [`QuicSettings`] picks the shape from config.
//!
//! ## Trust model: mutual TLS
//!
//! Every node presents a certificate (a [`NodeCertificate`]: operator-issued
//! PEM files, or self-signed) in both directions, and verifies the peer's:
//! the dialer verifies the server, the listener verifies the client. There is
//! no default trust — each side is told how to recognise its peers:
//!
//! - **CA roots**: [`QuicConfig::with_client_verification`] verifies servers
//!   dialed and clients accepted against the cluster CA (load a bundle with
//!   [`id_effect_platform::tls::load_ca_bundle`]);
//!   [`QuicConfig::with_client_cert_roots`] sets a separate CA for clients.
//! - **Pinned fingerprints**: [`QuicConfig::with_pinned_peer_cert_sha256`]
//!   accepts only peers whose leaf certificate has one of the given SHA-256
//!   fingerprints (of the DER; a node's own is [`QuicConfig::cert_sha256`]),
//!   in both directions. The zero-PKI cluster mode. One direction only:
//!   [`QuicConfig::with_pinned_server_cert_sha256`],
//!   [`QuicConfig::with_pinned_client_cert_sha256`].
//! - **Neither**: dialing fails with [`QuicConfigError::NoServerTrust`]
//!   ([`QuicTransport::try_new`] checks it up front) and listening fails with
//!   [`QuicConfigError::NoClientTrust`]. A client without a certificate, or
//!   with one the listener does not trust, is refused during the TLS
//!   handshake.
//!
//! Both opt-outs are named and recorded in the posture registry
//! ([`id_effect::observability::posture`], ADR 0013):
//! [`QuicConfig::dangerous_accept_any_server_cert`] and
//! [`QuicConfig::dangerous_no_client_auth`].
//!
//! Node authentication is the cookie handshake on top ([`crate::handshake`]),
//! whose proofs are bound to this TLS channel through the exporter secret
//! ([`Connection::channel_binding`]): TLS gives confidentiality, integrity and
//! both nodes' identities; the cookie gives cluster membership.
//!
//! With roots, the dialer checks the server certificate against
//! [`QuicConfig::server_name`] (default [`DEFAULT_SERVER_NAME`],
//! `"cluster"`), so every node's certificate must carry that name as a
//! subject alternative name — either issue all node certificates for
//! `"cluster"`, or set the name with [`QuicConfig::with_server_name`] to
//! match the certificates you issue (e.g. [`QuicConfig::self_signed`]
//! certifies the `host` it is given). Client certificates are checked
//! against the CA only, not against a name.
//!
//! ## Rotation
//!
//! [`QuicConfig::from_reloader`] takes its certificate from a
//! [`CertReloader`]: once the reloader has seen new files, every new
//! connection, dialed or accepted, presents the new certificate. Listeners
//! keep running; open connections keep the certificate they started with.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use id_effect_platform::quic::{
  AlpnRoute, QuicEndpoint, QuicEndpointConfig, QuicError, QuicLimits, Route, client_config,
};
use id_effect_platform::tls::{self as certs, CertError, CertReloader, Identity, NodeCertificate};
use quinn::{Endpoint, RecvStream, SendStream};
use tokio::sync::Mutex;

pub use id_effect_platform::quic::ALPN_MESH;

use super::{Connection, Listener, Transport, TransportError};
use crate::handshake::CHANNEL_BINDING_LABEL;
use crate::transport::tcp::{MAX_FRAME_BYTES, frame_within_limit};

/// Certificate material and peer trust for one node.
pub struct QuicConfig {
  /// The certificate this node presents, as server and as client.
  identity: Identity,
  /// Roots used to verify the servers this node dials.
  roots: Option<rustls::RootCertStore>,
  /// SHA-256 fingerprints (of the DER) of the only server leaf
  /// certificates the dialer accepts; empty = no pinning.
  pinned: Vec<[u8; 32]>,
  /// Explicit opt-out: with neither roots nor pins, accept any server.
  accept_any: bool,
  /// TLS server name the dialer presents and verifies the peer against.
  server_name: String,
  /// Roots used to verify the clients this node accepts.
  client_roots: Option<rustls::RootCertStore>,
  /// Fingerprints of the only client leaf certificates the listener
  /// accepts; empty = no pinning.
  client_pinned: Vec<[u8; 32]>,
  /// Explicit opt-out: accept clients without a certificate.
  no_client_auth: bool,
}

/// A QUIC configuration that cannot be used safely.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum QuicConfigError {
  /// Nothing tells the dialer which server certificates to trust.
  #[error(
    "quic: no server trust configured; set roots (with_client_verification), pin certificate \
     fingerprints (with_pinned_server_cert_sha256), or opt out with \
     dangerous_accept_any_server_cert"
  )]
  NoServerTrust,
  /// Nothing tells the listener which client certificates to trust.
  #[error(
    "quic: no client trust configured; set roots (with_client_verification or \
     with_client_cert_roots), pin certificate fingerprints (with_pinned_client_cert_sha256), or \
     opt out with dangerous_no_client_auth"
  )]
  NoClientTrust,
}

/// Server name used when none is configured (backward compatible).
pub const DEFAULT_SERVER_NAME: &str = "cluster";

/// Posture id of [`QuicConfig::dangerous_accept_any_server_cert`].
pub const DANGEROUS_ACCEPT_ANY_SERVER_CERT: &str =
  "id_effect_node::QuicConfig::dangerous_accept_any_server_cert";

/// Posture id of [`QuicConfig::dangerous_no_client_auth`].
pub const DANGEROUS_NO_CLIENT_AUTH: &str = "id_effect_node::QuicConfig::dangerous_no_client_auth";

impl QuicConfig {
  /// Self-signed certificate for `host` — the zero-PKI cluster mode (peers
  /// pin its fingerprint).
  pub fn self_signed(host: &str) -> Result<Self, TransportError> {
    let cert = NodeCertificate::self_signed(host).map_err(|e| TransportError::Io(e.to_string()))?;
    Self::from_certificate(&cert).map_err(|e| TransportError::Io(e.to_string()))
  }

  /// A config presenting `cert` (e.g. from
  /// [`NodeCertificate::from_pem_files`]). Fails when the crypto provider
  /// cannot use the key.
  pub fn from_certificate(cert: &NodeCertificate) -> Result<Self, CertError> {
    Ok(Self::from_identity(Identity::new(cert.certified_key()?)))
  }

  /// A config presenting whatever certificate `reloader` holds at the time
  /// of each new connection (see the module docs, "Rotation").
  pub fn from_reloader(reloader: &CertReloader) -> Self {
    Self::from_identity(reloader.identity())
  }

  fn from_identity(identity: Identity) -> Self {
    Self {
      identity,
      roots: None,
      pinned: Vec::new(),
      accept_any: false,
      server_name: DEFAULT_SERVER_NAME.to_string(),
      client_roots: None,
      client_pinned: Vec::new(),
      no_client_auth: false,
    }
  }

  /// SHA-256 fingerprint of this node's current leaf certificate (of its
  /// DER encoding) — the value peers pass to
  /// [`QuicConfig::with_pinned_peer_cert_sha256`].
  pub fn cert_sha256(&self) -> [u8; 32] {
    self.identity.cert_sha256()
  }

  /// Accept only servers whose leaf certificate has this SHA-256
  /// fingerprint (of the DER). Call once per trusted certificate. The pin
  /// stands in for name verification; combined with
  /// [`QuicConfig::with_client_verification`], both checks must pass.
  pub fn with_pinned_server_cert_sha256(mut self, fingerprint: [u8; 32]) -> Self {
    self.pinned.push(fingerprint);
    self
  }

  /// Accept only clients whose leaf certificate has this SHA-256
  /// fingerprint. Call once per trusted certificate; combined with client
  /// roots, both checks must pass.
  pub fn with_pinned_client_cert_sha256(mut self, fingerprint: [u8; 32]) -> Self {
    self.client_pinned.push(fingerprint);
    self
  }

  /// Pin a peer in both directions: as a server this node dials and as a
  /// client it accepts. In a pinned cluster, pin every node's certificate.
  pub fn with_pinned_peer_cert_sha256(self, fingerprint: [u8; 32]) -> Self {
    self
      .with_pinned_server_cert_sha256(fingerprint)
      .with_pinned_client_cert_sha256(fingerprint)
  }

  /// **Dangerous.** Dial servers without verifying their certificate when
  /// no roots or pins are set (they win when present). Recorded in the
  /// posture registry as [`DANGEROUS_ACCEPT_ANY_SERVER_CERT`].
  ///
  /// This re-opens active man-in-the-middle attacks on the TLS layer: any
  /// on-path host can terminate the connection with a certificate of its
  /// own. The cookie handshake's channel binding still keeps it from
  /// relaying a proof, so it cannot complete a session without the cookie,
  /// but the server's identity and the session's confidentiality then rest
  /// on the cookie alone — whoever holds or leaks it can intercept every
  /// dial. Use pins or roots in production.
  pub fn dangerous_accept_any_server_cert(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_ACCEPT_ANY_SERVER_CERT,
      "H3: the QUIC dialer accepts any server certificate, so an on-path host can terminate the \
       mesh TLS session",
    );
    self.accept_any = true;
    self
  }

  /// **Dangerous.** Accept clients without verifying (or even requesting)
  /// a certificate when no client roots or pins are set (they win when
  /// present). Recorded in the posture registry as
  /// [`DANGEROUS_NO_CLIENT_AUTH`].
  ///
  /// The listener then cannot tell a cluster node from any host that can
  /// reach the port: membership rests on the cookie alone, and every
  /// stranger gets a TLS session and a handshake attempt.
  pub fn dangerous_no_client_auth(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_NO_CLIENT_AUTH,
      "dist.tls#4: the QUIC listener accepts peers without a client certificate; cluster \
       membership rests on the cookie alone",
    );
    self.no_client_auth = true;
    self
  }

  /// Check that the dialer has a way to authenticate servers.
  fn check_trust(&self) -> Result<(), QuicConfigError> {
    if self.roots.is_none() && self.pinned.is_empty() && !self.accept_any {
      return Err(QuicConfigError::NoServerTrust);
    }
    Ok(())
  }

  /// Check that the listener has a way to authenticate clients.
  fn check_client_trust(&self) -> Result<(), QuicConfigError> {
    if self.client_roots.is_none() && self.client_pinned.is_empty() && !self.no_client_auth {
      return Err(QuicConfigError::NoClientTrust);
    }
    Ok(())
  }

  /// Verify peers against the given roots (the cluster CA): the servers
  /// this node dials, whose certificates must be valid for
  /// [`QuicConfig::server_name`], and the clients it accepts (unless
  /// [`QuicConfig::with_client_cert_roots`] sets other roots for them).
  pub fn with_client_verification(mut self, roots: rustls::RootCertStore) -> Self {
    if self.client_roots.is_none() {
      self.client_roots = Some(roots.clone());
    }
    self.roots = Some(roots);
    self
  }

  /// Verify the clients this node accepts against these roots only.
  pub fn with_client_cert_roots(mut self, roots: rustls::RootCertStore) -> Self {
    self.client_roots = Some(roots);
    self
  }

  /// Set the TLS server name dialed connections present and verify
  /// (default [`DEFAULT_SERVER_NAME`]).
  pub fn with_server_name(mut self, name: impl Into<String>) -> Self {
    self.server_name = name.into();
    self
  }

  /// The TLS server name dialed connections present and verify.
  pub fn server_name(&self) -> &str {
    &self.server_name
  }
}

struct QuicConnection {
  send: Mutex<SendStream>,
  recv: Mutex<RecvStream>,
  conn: quinn::Connection,
  /// Largest frame `recv` accepts (see `Connection::set_max_frame_bytes`).
  max_frame: AtomicU32,
}

impl QuicConnection {
  fn new(send: SendStream, recv: RecvStream, conn: quinn::Connection) -> Self {
    Self {
      send: Mutex::new(send),
      recv: Mutex::new(recv),
      conn,
      max_frame: AtomicU32::new(MAX_FRAME_BYTES),
    }
  }
}

#[async_trait::async_trait]
impl Connection for QuicConnection {
  async fn send(&self, frame: Bytes) -> Result<(), TransportError> {
    if frame.len() as u64 > MAX_FRAME_BYTES as u64 {
      return Err(TransportError::Io(
        "frame exceeds MAX_FRAME_BYTES".to_string(),
      ));
    }
    let mut send = self.send.lock().await;
    send
      .write_all(&(frame.len() as u32).to_be_bytes())
      .await
      .map_err(|_| TransportError::Closed)?;
    send
      .write_all(&frame)
      .await
      .map_err(|_| TransportError::Closed)?;
    Ok(())
  }

  async fn recv(&self) -> Result<Bytes, TransportError> {
    let mut recv = self.recv.lock().await;
    let mut len_buf = [0u8; 4];
    recv
      .read_exact(&mut len_buf)
      .await
      .map_err(|_| TransportError::Closed)?;
    let len = u32::from_be_bytes(len_buf);
    frame_within_limit(len, self.max_frame.load(Ordering::Acquire))?;
    let mut payload = vec![0u8; len as usize];
    recv
      .read_exact(&mut payload)
      .await
      .map_err(|_| TransportError::Closed)?;
    Ok(Bytes::from(payload))
  }

  async fn close(&self) {
    self.conn.close(0u32.into(), b"close");
  }

  fn peer_addr(&self) -> String {
    self.conn.remote_address().to_string()
  }

  fn set_max_frame_bytes(&self, max: u32) {
    self
      .max_frame
      .store(max.min(MAX_FRAME_BYTES), Ordering::Release);
  }

  fn channel_binding(&self) -> Option<[u8; 32]> {
    let mut binding = [0u8; 32];
    self
      .conn
      .export_keying_material(&mut binding, CHANNEL_BINDING_LABEL, b"")
      .ok()?;
    Some(binding)
  }
}

/// The `idm/1` route of an endpoint: connections that passed the route's
/// client-certificate policy. A refused attempt (failed handshake, no verified
/// client chain) is a [`TransportError::Connect`]; keep accepting.
struct QuicListener {
  route: AlpnRoute,
  /// Keeps the endpoint accepting while the listener lives.
  endpoint: QuicEndpoint,
  addr: String,
}

#[async_trait::async_trait]
impl Listener for QuicListener {
  async fn accept(&self) -> Result<Box<dyn Connection>, TransportError> {
    let conn = match self.route.accept().await {
      None => return Err(TransportError::Closed),
      Some(Err(refused)) => return Err(TransportError::Connect(refused.to_string())),
      Some(Ok(conn)) => conn,
    };
    // A peer that completes the handshake but opens no stream cannot hold the
    // accept loop longer than a handshake may take.
    let opened =
      tokio::time::timeout(self.endpoint.limits().handshake_timeout, conn.accept_bi()).await;
    match opened {
      Ok(Ok((send, recv))) => Ok(Box::new(QuicConnection::new(send, recv, conn))),
      Ok(Err(e)) => Err(TransportError::Connect(e.to_string())),
      Err(_) => {
        conn.close(0u32.into(), b"no stream");
        Err(TransportError::Connect(
          "quic: the peer opened no stream in time".to_string(),
        ))
      }
    }
  }

  fn local_addr(&self) -> String {
    self.addr.clone()
  }
}

/// QUIC transport (quinn + rustls). See the module docs for the trust
/// model and the two endpoint shapes.
pub struct QuicTransport {
  config: Arc<QuicConfig>,
  /// The shared endpoint ([`QuicTransport::on_endpoint`]); `None` binds an
  /// own endpoint per listener and dials from a fresh client socket.
  endpoint: Option<QuicEndpoint>,
}

impl QuicTransport {
  /// Transport from certificate material, without checking it. It listens on
  /// an own endpoint that serves only `idm/1`, with handshake-level mutual TLS.
  ///
  /// A config with no server trust (no roots, no pins, no
  /// [`QuicConfig::dangerous_accept_any_server_cert`]) can still listen, but
  /// every dial fails with [`TransportError::Connect`] naming
  /// [`QuicConfigError::NoServerTrust`]. Likewise, without client trust,
  /// [`Transport::listen`] fails with [`TransportError::Bind`] naming
  /// [`QuicConfigError::NoClientTrust`]. Prefer [`QuicTransport::try_new`].
  pub fn new(config: QuicConfig) -> Self {
    Self {
      config: Arc::new(config),
      endpoint: None,
    }
  }

  /// Transport from certificate material, refusing a config that gives
  /// the dialer no way to authenticate servers. Client trust is checked
  /// when the transport listens (a dial-only transport needs none).
  pub fn try_new(config: QuicConfig) -> Result<Self, QuicConfigError> {
    config.check_trust()?;
    Ok(Self::new(config))
  }

  /// The mesh on a shared endpoint (ADR 0014 D7): [`Transport::listen`]
  /// registers the `idm/1` route on `endpoint` (the address it is given is
  /// ignored; the listener reports the endpoint's), and dials leave from the
  /// endpoint's socket. The route presents this config's certificate and
  /// refuses, before any frame is read, every connection whose client chain
  /// the handshake did not verify against this config's client trust. The
  /// trust checks of [`QuicTransport::new`] apply.
  pub fn on_endpoint(config: QuicConfig, endpoint: &QuicEndpoint) -> Self {
    Self {
      config: Arc::new(config),
      endpoint: Some(endpoint.clone()),
    }
  }

  fn client_config(&self, limits: &QuicLimits) -> Result<quinn::ClientConfig, TransportError> {
    self
      .config
      .check_trust()
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    let tls = certs::client_tls_config(
      &self.config.identity,
      self.config.roots.as_ref(),
      &self.config.pinned,
    )
    .map_err(|e| TransportError::Io(e.to_string()))?;
    client_config(tls, &[ALPN_MESH], limits).map_err(|e| TransportError::Io(e.to_string()))
  }

  /// The `idm/1` route: verified clients only, unless the config opted out
  /// with `dangerous_no_client_auth` and has no client trust.
  fn mesh_route(&self) -> Result<Route, TransportError> {
    self
      .config
      .check_client_trust()
      .map_err(|e| TransportError::Bind(e.to_string()))?;
    let route = Route::new(ALPN_MESH).with_identity(self.config.identity.clone());
    if self.config.client_roots.is_none() && self.config.client_pinned.is_empty() {
      // Only behind `dangerous_no_client_auth` (checked above).
      return Ok(route);
    }
    Ok(route.require_verified_client(
      self.config.client_roots.clone(),
      self.config.client_pinned.clone(),
    ))
  }
}

fn bind_error(e: QuicError) -> TransportError {
  TransportError::Bind(e.to_string())
}

#[async_trait::async_trait]
impl Transport for QuicTransport {
  async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>, TransportError> {
    let remote: SocketAddr = addr
      .parse()
      .map_err(|e| TransportError::Connect(format!("bad addr {addr}: {e}")))?;
    let conn = match &self.endpoint {
      Some(endpoint) => endpoint
        .connect(
          remote,
          &self.config.server_name,
          self.client_config(endpoint.limits())?,
        )
        .await
        .map_err(|e| TransportError::Connect(e.to_string()))?,
      None => {
        let bind: SocketAddr = if remote.is_ipv6() {
          "[::]:0".parse().expect("valid")
        } else {
          "0.0.0.0:0".parse().expect("valid")
        };
        let endpoint =
          Endpoint::client(bind).map_err(|e| TransportError::Connect(e.to_string()))?;
        endpoint
          .connect_with(
            self.client_config(&QuicLimits::default())?,
            remote,
            &self.config.server_name,
          )
          .map_err(|e| TransportError::Connect(e.to_string()))?
          .await
          .map_err(|e| TransportError::Connect(e.to_string()))?
      }
    };
    let (send, recv) = conn
      .open_bi()
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    Ok(Box::new(QuicConnection::new(send, recv, conn)))
  }

  async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>, TransportError> {
    let route = self.mesh_route()?;
    let endpoint = match &self.endpoint {
      Some(endpoint) => endpoint.clone(),
      None => {
        let bind: SocketAddr = addr
          .parse()
          .map_err(|e| TransportError::Bind(format!("bad addr {addr}: {e}")))?;
        QuicEndpoint::bind(bind, QuicEndpointConfig::new(self.config.identity.clone()))
          .map_err(bind_error)?
      }
    };
    let route = endpoint.register(route).map_err(bind_error)?;
    Ok(Box::new(QuicListener {
      route,
      addr: endpoint.local_addr().to_string(),
      endpoint,
    }))
  }
}

/// The `quic` section of a node's config.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuicSettings {
  /// `quic.mesh_port`: serve the mesh on its own UDP port, with
  /// handshake-level mutual TLS, instead of on the endpoint shared with
  /// HTTP/3 (ADR 0014 D7). `Some(0)` picks an ephemeral port.
  pub mesh_port: Option<u16>,
}

impl QuicSettings {
  /// The mesh transport for these settings and the address to
  /// [`listen`](Transport::listen) on: the `shared` endpoint by default, or
  /// with `mesh_port` an own endpoint on the shared endpoint's IP and that
  /// port.
  pub fn mesh_transport(
    &self,
    config: QuicConfig,
    shared: &QuicEndpoint,
  ) -> (QuicTransport, String) {
    match self.mesh_port {
      None => (
        QuicTransport::on_endpoint(config, shared),
        shared.local_addr().to_string(),
      ),
      Some(port) => (
        QuicTransport::new(config),
        SocketAddr::new(shared.local_addr().ip(), port).to_string(),
      ),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn server_name_defaults_to_cluster() {
    let config = QuicConfig::self_signed("node-a").expect("cert");
    assert_eq!(config.server_name(), DEFAULT_SERVER_NAME);
    assert_eq!(DEFAULT_SERVER_NAME, "cluster");
  }

  #[test]
  fn server_name_is_configurable() {
    let config = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_server_name("node-a.cluster.local");
    assert_eq!(config.server_name(), "node-a.cluster.local");
  }

  fn roots_for(config: &QuicConfig) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots
      .add(config.identity.current().cert[0].clone())
      .expect("root");
    roots
  }

  /// Dial `server` with `client`; on success, check one frame crosses. The
  /// listener pins the dialer's certificate (client-auth failures are in
  /// `tests/transport_tls.rs`).
  async fn dial(server: QuicConfig, client: QuicConfig) -> Result<(), TransportError> {
    let server = server.with_pinned_client_cert_sha256(client.cert_sha256());
    let listener = QuicTransport::new(server)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr();
    let server_side = tokio::spawn(async move {
      let conn = listener.accept().await?;
      conn.recv().await
    });
    let dialed = async {
      let conn = QuicTransport::new(client).connect(&addr).await?;
      conn.send(Bytes::from_static(b"hi")).await?;
      Ok::<_, TransportError>(conn)
    }
    .await;
    let conn = match dialed {
      Ok(conn) => conn,
      Err(e) => {
        server_side.abort();
        return Err(e);
      }
    };
    let frame = server_side.await.expect("join")?;
    assert_eq!(frame, Bytes::from_static(b"hi"));
    drop(conn);
    Ok(())
  }

  #[tokio::test]
  async fn default_mode_refuses_to_dial() {
    let server = QuicConfig::self_signed("node-b").expect("cert");
    let client = QuicConfig::self_signed("node-a").expect("cert");
    assert!(matches!(
      QuicTransport::try_new(QuicConfig::self_signed("node-a").expect("cert")),
      Err(QuicConfigError::NoServerTrust)
    ));
    assert!(matches!(
      dial(server, client).await,
      Err(TransportError::Connect(m)) if m.contains("no server trust")
    ));
  }

  #[tokio::test]
  async fn dangerous_opt_out_accepts_any_server_certificate() {
    let server = QuicConfig::self_signed("node-b").expect("cert");
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .dangerous_accept_any_server_cert();
    dial(server, client).await.expect("handshake");
  }

  #[tokio::test]
  async fn pinned_fingerprint_accepts_only_that_certificate() {
    let server = QuicConfig::self_signed("node-b").expect("cert");
    let pin = server.cert_sha256();
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_pinned_server_cert_sha256(pin);
    dial(server, client).await.expect("pinned handshake");

    let server = QuicConfig::self_signed("node-b").expect("cert");
    let impostor_pin = QuicConfig::self_signed("node-b")
      .expect("cert")
      .cert_sha256();
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_pinned_server_cert_sha256(impostor_pin)
      .dangerous_accept_any_server_cert();
    assert!(
      matches!(dial(server, client).await, Err(TransportError::Connect(_))),
      "a pin wins over the opt-out"
    );
  }

  #[tokio::test]
  async fn pins_and_roots_must_both_pass() {
    let server = QuicConfig::self_signed("node-b").expect("cert");
    let (roots, pin) = (roots_for(&server), server.cert_sha256());
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_client_verification(roots)
      .with_pinned_server_cert_sha256(pin)
      .with_server_name("node-b");
    dial(server, client).await.expect("roots and pin");

    let server = QuicConfig::self_signed("node-b").expect("cert");
    let (roots, pin) = (roots_for(&server), server.cert_sha256());
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_client_verification(roots)
      .with_pinned_server_cert_sha256(pin);
    assert!(
      matches!(dial(server, client).await, Err(TransportError::Connect(_))),
      "the pin does not excuse a name the roots reject"
    );
  }

  #[test]
  fn fingerprints_differ_per_certificate() {
    let a = QuicConfig::self_signed("node-a").expect("cert");
    let b = QuicConfig::self_signed("node-a").expect("cert");
    assert_eq!(a.cert_sha256(), a.cert_sha256());
    assert_ne!(a.cert_sha256(), b.cert_sha256());
  }

  #[tokio::test]
  async fn both_ends_derive_the_same_channel_binding() {
    let client = QuicConfig::self_signed("node-a").expect("cert");
    let server = QuicConfig::self_signed("node-b")
      .expect("cert")
      .with_pinned_peer_cert_sha256(client.cert_sha256());
    let client = client.with_pinned_peer_cert_sha256(server.cert_sha256());
    let listener = QuicTransport::new(server)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr();
    let client = QuicTransport::try_new(client).expect("trusted");
    let (dialed, accepted) = tokio::join!(
      async {
        let conn = client.connect(&addr).await.expect("dial");
        conn.send(Bytes::from_static(b"hi")).await.expect("send");
        conn
      },
      async { listener.accept().await.expect("accept") }
    );
    let (ours, theirs) = (dialed.channel_binding(), accepted.channel_binding());
    assert!(ours.is_some());
    assert_eq!(ours, theirs);
  }

  #[tokio::test]
  async fn pinned_roots_verify_against_the_configured_server_name() {
    let server = QuicConfig::self_signed("node-b").expect("cert");
    let roots = roots_for(&server);
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_client_verification(roots)
      .with_server_name("node-b");
    dial(server, client)
      .await
      .expect("handshake with matching name");
  }

  /// Regression: the dialer always presented "cluster", so pinning failed
  /// for any certificate not issued for that exact name.
  #[tokio::test]
  async fn pinned_roots_reject_a_mismatched_server_name() {
    let server = QuicConfig::self_signed("node-b").expect("cert");
    let roots = roots_for(&server);
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .with_client_verification(roots);
    assert!(matches!(
      dial(server, client).await,
      Err(TransportError::Connect(_))
    ));
  }

  #[tokio::test]
  async fn listen_without_client_trust_is_refused() {
    let bare = QuicConfig::self_signed("node-b").expect("cert");
    let err = QuicTransport::new(bare)
      .listen("127.0.0.1:0")
      .await
      .err()
      .expect("no client trust, no listener");
    assert!(
      matches!(&err, TransportError::Bind(m) if m.contains("no client trust")),
      "{err}"
    );
  }

  #[tokio::test]
  async fn no_client_auth_is_an_explicit_opt_out() {
    let server = QuicConfig::self_signed("node-b")
      .expect("cert")
      .dangerous_no_client_auth();
    let pin = server.cert_sha256();
    let listener = QuicTransport::new(server)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr();
    let client = QuicTransport::try_new(
      QuicConfig::self_signed("node-a")
        .expect("cert")
        .with_pinned_server_cert_sha256(pin),
    )
    .expect("trusted");
    let (dialed, accepted) = tokio::join!(
      async {
        let conn = client.connect(&addr).await.expect("dial");
        conn.send(Bytes::from_static(b"hi")).await.expect("send");
        conn
      },
      async { listener.accept().await.expect("accept") }
    );
    assert_eq!(
      accepted.recv().await.expect("recv"),
      Bytes::from_static(b"hi")
    );
    drop(dialed);
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == DANGEROUS_NO_CLIENT_AUTH)
    );
  }
}
