//! [`QuicTransport`] — quinn + rustls (feature `quic`).
//!
//! Each logical connection is one QUIC connection with a single
//! bidirectional control stream carrying length-delimited frames (the
//! same layout as the TCP transport). Multiplexed per-process-pair
//! streams are a wire-compatible upgrade reserved for a later phase.
//!
//! ## Trust model
//!
//! Each node generates (or is given) a certificate, and the dialer must be
//! told how to recognise the servers it dials — there is no default:
//!
//! - **Pinned fingerprints**: [`QuicConfig::with_pinned_server_cert_sha256`]
//!   accepts only servers whose leaf certificate has one of the given
//!   SHA-256 fingerprints (of the DER; a node's own is
//!   [`QuicConfig::cert_sha256`]). The zero-PKI cluster mode.
//! - **Roots**: [`QuicConfig::with_client_verification`] (below).
//! - **Neither**: [`QuicTransport::try_new`] fails with
//!   [`QuicConfigError::NoServerTrust`] (and a transport built with
//!   [`QuicTransport::new`] refuses to dial). Accepting any certificate takes
//!   the explicit [`QuicConfig::dangerous_accept_any_server_cert`].
//!
//! Node authentication is the cookie handshake on top ([`crate::handshake`]),
//! whose proofs are bound to this TLS channel through the exporter secret
//! ([`Connection::channel_binding`]): TLS gives confidentiality, integrity and
//! server identity; the cookie gives cluster membership. The transport is not
//! mutual TLS — there is no client certificate.
//!
//! To verify servers against your own roots, use
//! [`QuicConfig::with_client_verification`]. The dialer then checks the
//! peer certificate against [`QuicConfig::server_name`] (default
//! [`DEFAULT_SERVER_NAME`], `"cluster"`), so every node's certificate must
//! carry that name as a subject alternative name — either issue all node
//! certificates for `"cluster"`, or set the name with
//! [`QuicConfig::with_server_name`] to match the certificates you issue
//! (e.g. [`QuicConfig::self_signed`] certifies the `host` it is given).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bytes::Bytes;
use quinn::{ClientConfig, Endpoint, RecvStream, SendStream, ServerConfig};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::{Connection, Listener, Transport, TransportError};
use crate::handshake::CHANNEL_BINDING_LABEL;
use crate::transport::tcp::{MAX_FRAME_BYTES, frame_within_limit};

/// Certificate material for one node.
pub struct QuicConfig {
  cert_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
  key: rustls::pki_types::PrivateKeyDer<'static>,
  /// Roots used to verify the servers this node dials.
  roots: Option<rustls::RootCertStore>,
  /// SHA-256 fingerprints (of the DER) of the only server leaf
  /// certificates the dialer accepts; empty = no pinning.
  pinned: Vec<[u8; 32]>,
  /// Explicit opt-out: with neither roots nor pins, accept any server.
  accept_any: bool,
  /// TLS server name the dialer presents and verifies the peer against.
  server_name: String,
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
}

/// Server name used when none is configured (backward compatible).
pub const DEFAULT_SERVER_NAME: &str = "cluster";

impl QuicConfig {
  /// Self-signed certificate for `host` — the zero-config cluster mode.
  pub fn self_signed(host: &str) -> Result<Self, TransportError> {
    let certified = rcgen::generate_simple_self_signed(vec![host.to_string()])
      .map_err(|e| TransportError::Io(format!("rcgen: {e}")))?;
    let cert = certified.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
      certified.key_pair.serialize_der(),
    ));
    Ok(Self {
      cert_chain: vec![cert],
      key,
      roots: None,
      pinned: Vec::new(),
      accept_any: false,
      server_name: DEFAULT_SERVER_NAME.to_string(),
    })
  }

  /// SHA-256 fingerprint of this node's leaf certificate (of its DER
  /// encoding) — the value peers pass to
  /// [`QuicConfig::with_pinned_server_cert_sha256`].
  pub fn cert_sha256(&self) -> [u8; 32] {
    self
      .cert_chain
      .first()
      .map(|cert| Sha256::digest(cert.as_ref()).into())
      .unwrap_or_default()
  }

  /// Accept only servers whose leaf certificate has this SHA-256
  /// fingerprint (of the DER). Call once per trusted certificate. The pin
  /// stands in for name verification; combined with
  /// [`QuicConfig::with_client_verification`], both checks must pass.
  pub fn with_pinned_server_cert_sha256(mut self, fingerprint: [u8; 32]) -> Self {
    self.pinned.push(fingerprint);
    self
  }

  /// **Dangerous.** Dial servers without verifying their certificate when
  /// no roots or pins are set (they win when present).
  ///
  /// This re-opens active man-in-the-middle attacks on the TLS layer: any
  /// on-path host can terminate the connection with a certificate of its
  /// own. The cookie handshake's channel binding still keeps it from
  /// relaying a proof, so it cannot complete a session without the cookie,
  /// but the server's identity and the session's confidentiality then rest
  /// on the cookie alone — whoever holds or leaks it can intercept every
  /// dial. Use pins or roots in production.
  pub fn dangerous_accept_any_server_cert(mut self) -> Self {
    self.accept_any = true;
    self
  }

  /// Check that the dialer has a way to authenticate servers.
  fn check_trust(&self) -> Result<(), QuicConfigError> {
    if self.roots.is_none() && self.pinned.is_empty() && !self.accept_any {
      return Err(QuicConfigError::NoServerTrust);
    }
    Ok(())
  }

  /// Verify the servers this node dials against the given roots. Peer
  /// certificates must be valid for [`QuicConfig::server_name`].
  pub fn with_client_verification(mut self, roots: rustls::RootCertStore) -> Self {
    self.roots = Some(roots);
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

/// Accepts any server certificate; only behind
/// [`QuicConfig::dangerous_accept_any_server_cert`]. Its signature checks
/// are shared with [`PinnedCert`].
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

/// Accepts only pinned leaf certificates, after the root check when roots
/// are configured too.
#[derive(Debug)]
struct PinnedCert {
  pins: Vec<[u8; 32]>,
  roots: Option<Arc<rustls::client::WebPkiServerVerifier>>,
  signatures: AcceptAnyCert,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCert {
  fn verify_server_cert(
    &self,
    end_entity: &rustls::pki_types::CertificateDer<'_>,
    intermediates: &[rustls::pki_types::CertificateDer<'_>],
    server_name: &rustls::pki_types::ServerName<'_>,
    ocsp: &[u8],
    now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
    if let Some(roots) = &self.roots {
      roots.verify_server_cert(end_entity, intermediates, server_name, ocsp, now)?;
    }
    let fingerprint: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
    if self.pins.contains(&fingerprint) {
      Ok(rustls::client::danger::ServerCertVerified::assertion())
    } else {
      Err(rustls::Error::InvalidCertificate(
        rustls::CertificateError::ApplicationVerificationFailure,
      ))
    }
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &rustls::pki_types::CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    self.signatures.verify_tls12_signature(message, cert, dss)
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &rustls::pki_types::CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    self.signatures.verify_tls13_signature(message, cert, dss)
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    self.signatures.supported_verify_schemes()
  }
}

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
  fn verify_server_cert(
    &self,
    _end_entity: &rustls::pki_types::CertificateDer<'_>,
    _intermediates: &[rustls::pki_types::CertificateDer<'_>],
    _server_name: &rustls::pki_types::ServerName<'_>,
    _ocsp: &[u8],
    _now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
    Ok(rustls::client::danger::ServerCertVerified::assertion())
  }

  fn verify_tls12_signature(
    &self,
    message: &[u8],
    cert: &rustls::pki_types::CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls12_signature(
      message,
      cert,
      dss,
      &self.0.signature_verification_algorithms,
    )
  }

  fn verify_tls13_signature(
    &self,
    message: &[u8],
    cert: &rustls::pki_types::CertificateDer<'_>,
    dss: &rustls::DigitallySignedStruct,
  ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
    rustls::crypto::verify_tls13_signature(
      message,
      cert,
      dss,
      &self.0.signature_verification_algorithms,
    )
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    self.0.signature_verification_algorithms.supported_schemes()
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

struct QuicListener {
  endpoint: Endpoint,
  addr: String,
}

#[async_trait::async_trait]
impl Listener for QuicListener {
  async fn accept(&self) -> Result<Box<dyn Connection>, TransportError> {
    let incoming = self.endpoint.accept().await.ok_or(TransportError::Closed)?;
    let conn = incoming
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    let (send, recv) = conn
      .accept_bi()
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    Ok(Box::new(QuicConnection::new(send, recv, conn)))
  }

  fn local_addr(&self) -> String {
    self.addr.clone()
  }
}

/// QUIC transport (quinn + rustls). See the module docs for the trust
/// model.
pub struct QuicTransport {
  config: Arc<QuicConfig>,
}

impl QuicTransport {
  /// Transport from certificate material, without checking it.
  ///
  /// A config with no server trust (no roots, no pins, no
  /// [`QuicConfig::dangerous_accept_any_server_cert`]) can still listen, but
  /// every dial fails with [`TransportError::Connect`] naming
  /// [`QuicConfigError::NoServerTrust`]. Prefer [`QuicTransport::try_new`].
  pub fn new(config: QuicConfig) -> Self {
    Self {
      config: Arc::new(config),
    }
  }

  /// Transport from certificate material, refusing a config that gives
  /// the dialer no way to authenticate servers.
  pub fn try_new(config: QuicConfig) -> Result<Self, QuicConfigError> {
    config.check_trust()?;
    Ok(Self::new(config))
  }

  /// The process default provider if one is installed, else aws-lc-rs (the
  /// provider the rest of the workspace compiles in). Configs are always
  /// built with an explicit provider.
  fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    rustls::crypto::CryptoProvider::get_default()
      .cloned()
      .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
  }

  fn client_config(&self) -> Result<ClientConfig, TransportError> {
    self
      .config
      .check_trust()
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    let provider = Self::provider();
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
      .with_protocol_versions(&[&rustls::version::TLS13])
      .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
    let tls = if !self.config.pinned.is_empty() {
      let roots = match &self.config.roots {
        Some(roots) => Some(
          rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots.clone()),
            Arc::clone(&provider),
          )
          .build()
          .map_err(|e| TransportError::Io(format!("tls: {e}")))?,
        ),
        None => None,
      };
      builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert {
          pins: self.config.pinned.clone(),
          roots,
          signatures: AcceptAnyCert(provider),
        }))
        .with_no_client_auth()
    } else if let Some(roots) = &self.config.roots {
      builder
        .with_root_certificates(roots.clone())
        .with_no_client_auth()
    } else {
      builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth()
    };
    let quic_tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
      .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
    Ok(ClientConfig::new(Arc::new(quic_tls)))
  }

  fn server_config(&self) -> Result<ServerConfig, TransportError> {
    let tls = rustls::ServerConfig::builder_with_provider(Self::provider())
      .with_protocol_versions(&[&rustls::version::TLS13])
      .map_err(|e| TransportError::Io(format!("tls: {e}")))?
      .with_no_client_auth()
      .with_single_cert(self.config.cert_chain.clone(), self.config.key.clone_key())
      .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
      .map_err(|e| TransportError::Io(format!("tls: {e}")))?;
    Ok(ServerConfig::with_crypto(Arc::new(quic_tls)))
  }
}

#[async_trait::async_trait]
impl Transport for QuicTransport {
  async fn connect(&self, addr: &str) -> Result<Box<dyn Connection>, TransportError> {
    let remote: std::net::SocketAddr = addr
      .parse()
      .map_err(|e| TransportError::Connect(format!("bad addr {addr}: {e}")))?;
    let bind: std::net::SocketAddr = if remote.is_ipv6() {
      "[::]:0".parse().expect("valid")
    } else {
      "0.0.0.0:0".parse().expect("valid")
    };
    let mut endpoint =
      Endpoint::client(bind).map_err(|e| TransportError::Connect(e.to_string()))?;
    endpoint.set_default_client_config(self.client_config()?);
    let conn = endpoint
      .connect(remote, &self.config.server_name)
      .map_err(|e| TransportError::Connect(e.to_string()))?
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    let (send, recv) = conn
      .open_bi()
      .await
      .map_err(|e| TransportError::Connect(e.to_string()))?;
    Ok(Box::new(QuicConnection::new(send, recv, conn)))
  }

  async fn listen(&self, addr: &str) -> Result<Box<dyn Listener>, TransportError> {
    let bind: std::net::SocketAddr = addr
      .parse()
      .map_err(|e| TransportError::Bind(format!("bad addr {addr}: {e}")))?;
    let endpoint = Endpoint::server(self.server_config()?, bind)
      .map_err(|e| TransportError::Bind(e.to_string()))?;
    let local = endpoint
      .local_addr()
      .map(|a| a.to_string())
      .unwrap_or_else(|_| addr.to_string());
    Ok(Box::new(QuicListener {
      endpoint,
      addr: local,
    }))
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
    roots.add(config.cert_chain[0].clone()).expect("root");
    roots
  }

  /// Dial `server` with `client`; on success, check one frame crosses.
  async fn dial(server: QuicConfig, client: QuicConfig) -> Result<(), TransportError> {
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
    let server = QuicConfig::self_signed("node-b").expect("cert");
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
}
