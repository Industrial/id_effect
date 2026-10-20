//! TLS 1.3 with mutual authentication over TCP (feature `tls-tcp`): [`TlsTcpConfig`], dialed and
//! listened on through [`TcpTransport::tls`](super::tcp::TcpTransport::tls).
//!
//! Frames are the TCP transport's (4-byte big-endian length prefix + payload, capped at
//! [`MAX_FRAME_BYTES`](super::tcp::MAX_FRAME_BYTES)), carried inside a TLS 1.3 session. The
//! certificate material is [`certs`]: operator-issued PEM files ([`NodeCertificate`]), CA
//! bundles ([`certs::load_ca_bundle`]) and rotation ([`CertReloader`]).
//!
//! ## Trust model: mutual TLS
//!
//! The same as the QUIC transport's. Every node presents a certificate in both directions and
//! verifies the peer's; there is no default trust:
//!
//! - **CA roots**: [`TlsTcpConfig::with_client_verification`] verifies servers dialed and clients
//!   accepted against the cluster CA; [`TlsTcpConfig::with_client_cert_roots`] sets a separate CA
//!   for clients. Server certificates must be valid for [`TlsTcpConfig::server_name`] (default
//!   [`DEFAULT_SERVER_NAME`], `"cluster"`); client certificates are checked against the CA only.
//! - **Pinned fingerprints**: [`TlsTcpConfig::with_pinned_peer_cert_sha256`] accepts only peers
//!   whose leaf certificate has one of the given SHA-256 fingerprints, in both directions (one
//!   direction: [`TlsTcpConfig::with_pinned_server_cert_sha256`],
//!   [`TlsTcpConfig::with_pinned_client_cert_sha256`]).
//! - **Neither**: dialing fails with [`TlsTcpConfigError::NoServerTrust`]
//!   ([`TcpTransport::try_tls`](super::tcp::TcpTransport::try_tls) checks it up front) and
//!   listening with [`TlsTcpConfigError::NoClientTrust`]. A client without a certificate, or with
//!   one the listener does not trust, is refused during the TLS handshake.
//!
//! The opt-outs are named and recorded in the posture registry
//! ([`id_effect::observability::posture`], ADR 0013):
//! [`TlsTcpConfig::dangerous_accept_any_server_cert`] and
//! [`TlsTcpConfig::dangerous_no_client_auth`].
//!
//! Each connection exports a channel binding ([`Connection::channel_binding`], the TLS exporter
//! for [`CHANNEL_BINDING_LABEL`]), so the cookie handshake's proofs cannot be relayed onto
//! another TLS session.
//!
//! ## Accepting
//!
//! The listener runs TLS handshakes in the background, at most [`MAX_PENDING_TLS_HANDSHAKES`] at
//! a time, each bounded by [`TlsTcpConfig::tls_handshake_timeout`] (default
//! [`DEFAULT_TLS_HANDSHAKE_TIMEOUT`]), so a silent or slow peer cannot hold up the others.
//! [`Listener::accept`] yields them in completion order: a refused or timed-out handshake is a
//! [`TransportError::Connect`] (keep accepting), [`TransportError::Closed`] means the listener is
//! gone.
//!
//! ## Rotation
//!
//! [`TlsTcpConfig::from_reloader`] takes its certificate from a [`CertReloader`]: once the
//! reloader has seen new files, every new connection, dialed or accepted, presents the new
//! certificate. Open connections keep the certificate they started with.

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::certs::{self, CertError, CertReloader, Identity, NodeCertificate};
use super::tcp::FramedConnection;
use super::{Connection, Listener, TransportError};
use crate::handshake::CHANNEL_BINDING_LABEL;

/// Server name used when none is configured (the same as the QUIC transport's).
pub const DEFAULT_SERVER_NAME: &str = "cluster";

/// Default bound on one TLS handshake (see [`TlsTcpConfig::with_tls_handshake_timeout`]).
pub const DEFAULT_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Most TLS handshakes a listener runs (or holds unaccepted) at once; further TCP connections
/// wait in the kernel backlog.
pub const MAX_PENDING_TLS_HANDSHAKES: usize = 64;

/// Pause after a failed TCP `accept` (e.g. out of file descriptors) before the next.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// Posture id of [`TlsTcpConfig::dangerous_accept_any_server_cert`].
pub const DANGEROUS_ACCEPT_ANY_SERVER_CERT: &str =
  "id_effect_node::TlsTcpConfig::dangerous_accept_any_server_cert";

/// Posture id of [`TlsTcpConfig::dangerous_no_client_auth`].
pub const DANGEROUS_NO_CLIENT_AUTH: &str = "id_effect_node::TlsTcpConfig::dangerous_no_client_auth";

/// Certificate material and peer trust for one node on the TLS TCP transport.
pub struct TlsTcpConfig {
  /// The certificate this node presents, as server and as client.
  identity: Identity,
  /// Roots used to verify the servers this node dials.
  roots: Option<rustls::RootCertStore>,
  /// Fingerprints of the only server leaf certificates the dialer accepts; empty = no pinning.
  pinned: Vec<[u8; 32]>,
  /// Explicit opt-out: with neither roots nor pins, accept any server.
  accept_any: bool,
  /// TLS server name the dialer presents and verifies the peer against.
  server_name: String,
  /// Roots used to verify the clients this node accepts.
  client_roots: Option<rustls::RootCertStore>,
  /// Fingerprints of the only client leaf certificates the listener accepts; empty = no pinning.
  client_pinned: Vec<[u8; 32]>,
  /// Explicit opt-out: accept clients without a certificate.
  no_client_auth: bool,
  /// Bound on one TLS handshake, dialing or accepting.
  handshake_timeout: Duration,
}

impl std::fmt::Debug for TlsTcpConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TlsTcpConfig")
      .field("identity", &self.identity)
      .field("roots", &self.roots.as_ref().map(|r| r.len()))
      .field("pinned", &self.pinned.len())
      .field("accept_any", &self.accept_any)
      .field("server_name", &self.server_name)
      .field("client_roots", &self.client_roots.as_ref().map(|r| r.len()))
      .field("client_pinned", &self.client_pinned.len())
      .field("no_client_auth", &self.no_client_auth)
      .field("handshake_timeout", &self.handshake_timeout)
      .finish()
  }
}

/// A TLS TCP configuration that cannot be used safely.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TlsTcpConfigError {
  /// Nothing tells the dialer which server certificates to trust.
  #[error(
    "tls-tcp: no server trust configured; set roots (with_client_verification), pin certificate \
     fingerprints (with_pinned_server_cert_sha256), or opt out with \
     dangerous_accept_any_server_cert"
  )]
  NoServerTrust,
  /// Nothing tells the listener which client certificates to trust.
  #[error(
    "tls-tcp: no client trust configured; set roots (with_client_verification or \
     with_client_cert_roots), pin certificate fingerprints (with_pinned_client_cert_sha256), or \
     opt out with dangerous_no_client_auth"
  )]
  NoClientTrust,
}

impl TlsTcpConfig {
  /// Self-signed certificate for `host` — the zero-PKI cluster mode (peers pin its fingerprint).
  pub fn self_signed(host: &str) -> Result<Self, CertError> {
    Self::from_certificate(&NodeCertificate::self_signed(host)?)
  }

  /// A config presenting `cert` (e.g. from [`NodeCertificate::from_pem_files`]). Fails when the
  /// crypto provider cannot use the key.
  pub fn from_certificate(cert: &NodeCertificate) -> Result<Self, CertError> {
    Ok(Self::from_identity(Identity::new(cert.certified_key()?)))
  }

  /// A config presenting whatever certificate `reloader` holds at the time of each new
  /// connection (see the module docs, "Rotation").
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
      handshake_timeout: DEFAULT_TLS_HANDSHAKE_TIMEOUT,
    }
  }

  /// SHA-256 fingerprint of this node's current leaf certificate (of its DER encoding) — the
  /// value peers pass to [`TlsTcpConfig::with_pinned_peer_cert_sha256`].
  pub fn cert_sha256(&self) -> [u8; 32] {
    self.identity.cert_sha256()
  }

  /// Accept only servers whose leaf certificate has this SHA-256 fingerprint (of the DER). Call
  /// once per trusted certificate. Combined with [`TlsTcpConfig::with_client_verification`],
  /// both checks must pass.
  pub fn with_pinned_server_cert_sha256(mut self, fingerprint: [u8; 32]) -> Self {
    self.pinned.push(fingerprint);
    self
  }

  /// Accept only clients whose leaf certificate has this SHA-256 fingerprint. Call once per
  /// trusted certificate; combined with client roots, both checks must pass.
  pub fn with_pinned_client_cert_sha256(mut self, fingerprint: [u8; 32]) -> Self {
    self.client_pinned.push(fingerprint);
    self
  }

  /// Pin a peer in both directions: as a server this node dials and as a client it accepts. In
  /// a pinned cluster, pin every node's certificate.
  pub fn with_pinned_peer_cert_sha256(self, fingerprint: [u8; 32]) -> Self {
    self
      .with_pinned_server_cert_sha256(fingerprint)
      .with_pinned_client_cert_sha256(fingerprint)
  }

  /// **Dangerous.** Dial servers without verifying their certificate when no roots or pins are
  /// set (they win when present). Recorded in the posture registry as
  /// [`DANGEROUS_ACCEPT_ANY_SERVER_CERT`].
  ///
  /// Any on-path host can then terminate the connection with a certificate of its own. The
  /// cookie handshake's channel binding still keeps it from relaying a proof, but the server's
  /// identity and the session's confidentiality rest on the cookie alone.
  pub fn dangerous_accept_any_server_cert(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_ACCEPT_ANY_SERVER_CERT,
      "H3: the TLS TCP dialer accepts any server certificate, so an on-path host can terminate \
       the mesh TLS session",
    );
    self.accept_any = true;
    self
  }

  /// **Dangerous.** Accept clients without verifying (or requesting) a certificate when no
  /// client roots or pins are set (they win when present). Recorded in the posture registry as
  /// [`DANGEROUS_NO_CLIENT_AUTH`].
  ///
  /// The listener then cannot tell a cluster node from any host that can reach the port:
  /// membership rests on the cookie alone.
  pub fn dangerous_no_client_auth(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_NO_CLIENT_AUTH,
      "dist.tls#4: the TLS TCP listener accepts peers without a client certificate; cluster \
       membership rests on the cookie alone",
    );
    self.no_client_auth = true;
    self
  }

  /// Verify peers against the given roots (the cluster CA): the servers this node dials, whose
  /// certificates must be valid for [`TlsTcpConfig::server_name`], and the clients it accepts
  /// (unless [`TlsTcpConfig::with_client_cert_roots`] sets other roots for them).
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

  /// Set the TLS server name dialed connections present and verify (default
  /// [`DEFAULT_SERVER_NAME`]).
  pub fn with_server_name(mut self, name: impl Into<String>) -> Self {
    self.server_name = name.into();
    self
  }

  /// The TLS server name dialed connections present and verify.
  pub fn server_name(&self) -> &str {
    &self.server_name
  }

  /// Bound one TLS handshake, dialing or accepting (default [`DEFAULT_TLS_HANDSHAKE_TIMEOUT`]).
  pub fn with_tls_handshake_timeout(mut self, timeout: Duration) -> Self {
    self.handshake_timeout = timeout;
    self
  }

  /// The bound on one TLS handshake.
  pub fn tls_handshake_timeout(&self) -> Duration {
    self.handshake_timeout
  }

  /// Check that the dialer has a way to authenticate servers.
  pub(crate) fn check_trust(&self) -> Result<(), TlsTcpConfigError> {
    if self.roots.is_none() && self.pinned.is_empty() && !self.accept_any {
      return Err(TlsTcpConfigError::NoServerTrust);
    }
    Ok(())
  }

  /// Check that the listener has a way to authenticate clients.
  fn check_client_trust(&self) -> Result<(), TlsTcpConfigError> {
    if self.client_roots.is_none() && self.client_pinned.is_empty() && !self.no_client_auth {
      return Err(TlsTcpConfigError::NoClientTrust);
    }
    Ok(())
  }
}

/// The 32-byte TLS exporter secret of a finished handshake (`CHANNEL_BINDING_LABEL`, empty
/// context, as on QUIC).
macro_rules! channel_binding {
  ($conn:expr) => {
    $conn
      .export_keying_material([0u8; 32], CHANNEL_BINDING_LABEL, Some(b""))
      .ok()
  };
}

/// Dial `addr` and run the TLS handshake as client.
pub(crate) async fn connect(
  config: &TlsTcpConfig,
  addr: &str,
) -> Result<Box<dyn Connection>, TransportError> {
  config
    .check_trust()
    .map_err(|e| TransportError::Connect(e.to_string()))?;
  let tls = certs::client_tls_config(&config.identity, config.roots.as_ref(), &config.pinned)?;
  let name = ServerName::try_from(config.server_name.clone()).map_err(|e| {
    TransportError::Connect(format!(
      "tls-tcp: bad server name {}: {e}",
      config.server_name
    ))
  })?;
  let stream = TcpStream::connect(addr)
    .await
    .map_err(|e| TransportError::Connect(e.to_string()))?;
  stream.set_nodelay(true).ok();
  let peer = peer_of(&stream);
  let handshake = TlsConnector::from(Arc::new(tls)).connect(name, stream);
  let tls = match tokio::time::timeout(config.handshake_timeout, handshake).await {
    Ok(Ok(tls)) => tls,
    Ok(Err(e)) => return Err(TransportError::Connect(format!("tls-tcp: {peer}: {e}"))),
    Err(_) => {
      return Err(TransportError::Connect(format!(
        "tls-tcp: {peer}: handshake timed out"
      )));
    }
  };
  let binding = channel_binding!(tls.get_ref().1);
  let (read, write) = tokio::io::split(tls);
  Ok(Box::new(FramedConnection::new(read, write, peer, binding)))
}

/// Bind `addr` and accept TLS clients in the background (see the module docs, "Accepting").
pub(crate) async fn listen(
  config: &TlsTcpConfig,
  addr: &str,
) -> Result<Box<dyn Listener>, TransportError> {
  config
    .check_client_trust()
    .map_err(|e| TransportError::Bind(e.to_string()))?;
  let tls = certs::server_tls_config(
    &config.identity,
    config.client_roots.as_ref(),
    &config.client_pinned,
  )?;
  let listener = TcpListener::bind(addr)
    .await
    .map_err(|e| TransportError::Bind(e.to_string()))?;
  let local = listener
    .local_addr()
    .map(|a| a.to_string())
    .unwrap_or_else(|_| addr.to_string());
  let (tx, rx) = mpsc::channel(MAX_PENDING_TLS_HANDSHAKES);
  let task = tokio::spawn(accept_loop(
    listener,
    TlsAcceptor::from(Arc::new(tls)),
    config.handshake_timeout,
    tx,
  ));
  Ok(Box::new(TlsTcpListener {
    accepted: Mutex::new(rx),
    addr: local,
    task,
  }))
}

type Accepted = Result<Box<dyn Connection>, TransportError>;

/// Accept TCP connections and handshake each on its own task, at most
/// [`MAX_PENDING_TLS_HANDSHAKES`] in flight or waiting to be accepted.
async fn accept_loop(
  listener: TcpListener,
  acceptor: TlsAcceptor,
  timeout: Duration,
  tx: mpsc::Sender<Accepted>,
) {
  let slots = Arc::new(Semaphore::new(MAX_PENDING_TLS_HANDSHAKES));
  loop {
    let Ok(slot) = Arc::clone(&slots).acquire_owned().await else {
      return;
    };
    let stream = match listener.accept().await {
      Ok((stream, _)) => stream,
      Err(e) => {
        if tx
          .send(Err(TransportError::Io(e.to_string())))
          .await
          .is_err()
        {
          return;
        }
        tokio::time::sleep(ACCEPT_BACKOFF).await;
        continue;
      }
    };
    if tx.is_closed() {
      return;
    }
    stream.set_nodelay(true).ok();
    let (acceptor, tx) = (acceptor.clone(), tx.clone());
    tokio::spawn(async move {
      let peer = peer_of(&stream);
      let accepted = match tokio::time::timeout(timeout, acceptor.accept(stream)).await {
        Ok(Ok(tls)) => {
          let binding = channel_binding!(tls.get_ref().1);
          let (read, write) = tokio::io::split(tls);
          Ok(Box::new(FramedConnection::new(read, write, peer, binding)) as Box<dyn Connection>)
        }
        Ok(Err(e)) => Err(TransportError::Connect(format!("tls-tcp: {peer}: {e}"))),
        Err(_) => Err(TransportError::Connect(format!(
          "tls-tcp: {peer}: handshake timed out"
        ))),
      };
      let _ = tx.send(accepted).await;
      drop(slot);
    });
  }
}

fn peer_of(stream: &TcpStream) -> String {
  stream
    .peer_addr()
    .map(|a| a.to_string())
    .unwrap_or_else(|_| "unknown".to_string())
}

struct TlsTcpListener {
  accepted: Mutex<mpsc::Receiver<Accepted>>,
  addr: String,
  task: tokio::task::JoinHandle<()>,
}

impl Drop for TlsTcpListener {
  fn drop(&mut self) {
    self.task.abort();
  }
}

#[async_trait::async_trait]
impl Listener for TlsTcpListener {
  async fn accept(&self) -> Result<Box<dyn Connection>, TransportError> {
    self
      .accepted
      .lock()
      .await
      .recv()
      .await
      .unwrap_or(Err(TransportError::Closed))
  }

  fn local_addr(&self) -> String {
    self.addr.clone()
  }
}

#[cfg(test)]
mod tests {
  use bytes::Bytes;

  use super::*;
  use crate::transport::Transport;
  use crate::transport::tcp::TcpTransport;

  /// Two self-signed nodes pinning each other.
  fn pinned_pair() -> (TlsTcpConfig, TlsTcpConfig) {
    let a = TlsTcpConfig::self_signed("node-a").expect("cert");
    let b = TlsTcpConfig::self_signed("node-b").expect("cert");
    let (pin_a, pin_b) = (a.cert_sha256(), b.cert_sha256());
    (
      a.with_pinned_peer_cert_sha256(pin_b),
      b.with_pinned_peer_cert_sha256(pin_a),
    )
  }

  #[test]
  fn defaults() {
    let config = TlsTcpConfig::self_signed("node-a").expect("cert");
    assert_eq!(config.server_name(), DEFAULT_SERVER_NAME);
    assert_eq!(
      config.tls_handshake_timeout(),
      DEFAULT_TLS_HANDSHAKE_TIMEOUT
    );
    assert_eq!(config.check_trust(), Err(TlsTcpConfigError::NoServerTrust));
    assert_eq!(
      config.check_client_trust(),
      Err(TlsTcpConfigError::NoClientTrust)
    );
    let config = config
      .with_server_name("node-a.cluster.local")
      .with_tls_handshake_timeout(Duration::from_secs(1));
    assert_eq!(config.server_name(), "node-a.cluster.local");
    assert_eq!(config.tls_handshake_timeout(), Duration::from_secs(1));
    assert!(format!("{config:?}").contains("node-a.cluster.local"));
  }

  #[tokio::test]
  async fn pinned_peers_exchange_frames_with_a_shared_channel_binding() {
    let (a, b) = pinned_pair();
    let listener = TcpTransport::tls(b)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let dial = TcpTransport::tls(a)
      .connect(&listener.local_addr())
      .await
      .expect("dial");
    let accepted = listener.accept().await.expect("accept");
    dial.send(Bytes::from_static(b"hi")).await.expect("send");
    assert_eq!(
      accepted.recv().await.expect("recv"),
      Bytes::from_static(b"hi")
    );
    let binding = dial.channel_binding().expect("binding");
    assert_eq!(accepted.channel_binding(), Some(binding));
    assert!(dial.peer_addr().starts_with("127.0.0.1:"));
    dial.close().await;
    assert!(matches!(accepted.recv().await, Err(TransportError::Closed)));
  }

  #[tokio::test]
  async fn silent_peer_times_out_without_blocking_other_handshakes() {
    let (a, b) = pinned_pair();
    let b = b.with_tls_handshake_timeout(Duration::from_millis(200));
    let listener = TcpTransport::tls(b)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr();
    let _silent = TcpStream::connect(&addr).await.expect("raw dial");
    let dial = TcpTransport::tls(a).connect(&addr).await.expect("dial");
    let accepted = listener.accept().await.expect("the real peer first");
    dial.send(Bytes::from_static(b"x")).await.expect("send");
    assert_eq!(
      accepted.recv().await.expect("recv"),
      Bytes::from_static(b"x")
    );
    assert!(matches!(
      listener.accept().await,
      Err(TransportError::Connect(m)) if m.contains("timed out")
    ));
  }

  #[tokio::test]
  async fn dialing_without_server_trust_fails_before_connecting() {
    let config = TlsTcpConfig::self_signed("node-a").expect("cert");
    assert!(matches!(
      TcpTransport::try_tls(TlsTcpConfig::self_signed("node-a").expect("cert")),
      Err(TlsTcpConfigError::NoServerTrust)
    ));
    assert!(matches!(
      TcpTransport::tls(config).connect("127.0.0.1:1").await,
      Err(TransportError::Connect(m)) if m.contains("no server trust")
    ));
  }

  #[tokio::test]
  async fn bad_server_name_is_a_connect_error() {
    let (a, _) = pinned_pair();
    assert!(matches!(
      TcpTransport::tls(a.with_server_name("not a name!")).connect("127.0.0.1:1").await,
      Err(TransportError::Connect(m)) if m.contains("bad server name")
    ));
  }

  #[tokio::test]
  async fn bad_bind_address_is_a_bind_error() {
    let (_, b) = pinned_pair();
    assert!(matches!(
      TcpTransport::tls(b).listen("not-an-address").await.err(),
      Some(TransportError::Bind(_))
    ));
  }
}
