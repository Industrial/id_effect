//! ALPN routes, the client-certificate rule they imply (D7), and dispatch.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use rustls::pki_types::CertificateDer;
use rustls::sign::CertifiedKey;
use tokio::sync::mpsc;

use super::{
  ALPN_H3, ALPN_MESH, CLOSE_BUSY, CLOSE_CLIENT_CERT_REQUIRED, CLOSE_NO_ROUTE, CongestionControl,
  QuicError, alpn_text,
};
use crate::tls::{self, ClientAuth, Identity};

/// What a route asks of the client's certificate.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ClientCertPolicy {
  /// Clients with or without a certificate (`h3`). A certificate that is sent is still
  /// verified when the endpoint has a client trust.
  Ignore,
  /// Only clients whose chain was verified during the handshake against these roots and/or
  /// pins (`idm/1`). Without one the connection is closed with
  /// [`CLOSE_CLIENT_CERT_REQUIRED`] before the route's handler sees it.
  RequireVerified {
    /// CA roots client chains are verified against.
    roots: Option<rustls::RootCertStore>,
    /// SHA-256 fingerprints of the only client leaf certificates accepted (after the roots
    /// check when both are set).
    pinned: Vec<[u8; 32]>,
  },
}

/// One ALPN served by an endpoint: its certificate, its client-certificate policy and the
/// congestion control of its connections.
#[derive(Clone, Debug)]
pub struct Route {
  alpn: Vec<u8>,
  identity: Option<Identity>,
  policy: ClientCertPolicy,
  congestion: CongestionControl,
}

impl Route {
  /// A route for `alpn` presenting the endpoint's certificate, ignoring client certificates,
  /// on CUBIC.
  pub fn new(alpn: impl Into<Vec<u8>>) -> Self {
    Self {
      alpn: alpn.into(),
      identity: None,
      policy: ClientCertPolicy::Ignore,
      congestion: CongestionControl::Cubic,
    }
  }

  /// HTTP/3 ([`ALPN_H3`]) on [`CongestionControl::Bbr`] (ADR 0014 D9; quinn marks BBR
  /// experimental, [`Self::with_congestion_control`] picks CUBIC back).
  pub fn h3() -> Self {
    Self::new(ALPN_H3).with_congestion_control(CongestionControl::Bbr)
  }

  /// The mesh ([`ALPN_MESH`]) on CUBIC, requiring a client verified against `roots` and/or
  /// `pinned`.
  pub fn mesh(roots: Option<rustls::RootCertStore>, pinned: Vec<[u8; 32]>) -> Self {
    Self::new(ALPN_MESH).require_verified_client(roots, pinned)
  }

  /// Run `congestion` control on this route's connections. On an endpoint shared by several
  /// routes the handshake starts on CUBIC and switches once the ALPN is chosen, before any
  /// application data is sent.
  pub fn with_congestion_control(mut self, congestion: CongestionControl) -> Self {
    self.congestion = congestion;
    self
  }

  /// The congestion control of this route's connections.
  pub fn congestion_control(&self) -> CongestionControl {
    self.congestion
  }

  /// Present `identity` to clients that negotiate this ALPN (a mesh certificate for the
  /// cluster name while `h3` presents the public one). Reloads reach new connections.
  pub fn with_identity(mut self, identity: Identity) -> Self {
    self.identity = Some(identity);
    self
  }

  /// Require a client chain verified against `roots` and/or `pinned` (see
  /// [`ClientCertPolicy::RequireVerified`]).
  pub fn require_verified_client(
    mut self,
    roots: Option<rustls::RootCertStore>,
    pinned: Vec<[u8; 32]>,
  ) -> Self {
    self.policy = ClientCertPolicy::RequireVerified { roots, pinned };
    self
  }

  /// The ALPN.
  pub fn alpn(&self) -> &[u8] {
    &self.alpn
  }

  /// The client-certificate policy.
  pub fn policy(&self) -> &ClientCertPolicy {
    &self.policy
  }
}

/// How the endpoint's TLS handshake asks for client certificates, derived from its routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientAuthMode {
  /// No route requires a verified client: no certificate is requested.
  None,
  /// Every route requires a verified client: handshake-level mutual TLS.
  Required,
  /// Some routes do and some do not (the shared shape, D7): a certificate is requested as
  /// optional with no CA hints, verified when sent, and required by the routes that need it.
  Optional,
}

/// Why a connection attempt on a route did not reach its handler.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
  /// The TLS handshake failed after the ALPN was chosen (for example a client certificate
  /// from another CA, or a server certificate the client rejected), or timed out.
  Handshake {
    /// The client.
    remote: SocketAddr,
    /// What failed.
    reason: String,
  },
  /// The handshake completed without a verified client chain and the route requires one.
  ClientCertRequired {
    /// The client.
    remote: SocketAddr,
  },
}

impl std::fmt::Display for Refusal {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Handshake { remote, reason } => write!(f, "quic handshake with {remote}: {reason}"),
      Self::ClientCertRequired { remote } => write!(
        f,
        "quic: {remote} presented no verified client certificate; refused before any frame"
      ),
    }
  }
}

impl std::error::Error for Refusal {}

/// What a route's handler receives: a connection, or a refused attempt (for its logs).
type Delivery = Result<quinn::Connection, Refusal>;

/// The receiving end of a [`Route`]: connections that negotiated its ALPN and passed its
/// client-certificate policy. Dropping it makes the endpoint refuse its ALPN
/// ([`CLOSE_NO_ROUTE`]).
#[derive(Debug)]
pub struct AlpnRoute {
  alpn: Vec<u8>,
  rx: tokio::sync::Mutex<mpsc::Receiver<Delivery>>,
}

impl AlpnRoute {
  /// The ALPN.
  pub fn alpn(&self) -> &[u8] {
    &self.alpn
  }

  /// The next connection or refused attempt; `None` once the endpoint is gone.
  pub async fn accept(&self) -> Option<Result<quinn::Connection, Refusal>> {
    self.rx.lock().await.recv().await
  }
}

/// Endpoint counters (see [`EndpointStats`](super::EndpointStats)).
#[derive(Debug, Default)]
pub(crate) struct Counters {
  pub(crate) retries_sent: AtomicU64,
  pub(crate) accepted: AtomicU64,
  pub(crate) refused_client_cert: AtomicU64,
  pub(crate) refused_no_route: AtomicU64,
  pub(crate) refused_busy: AtomicU64,
  pub(crate) handshakes_failed: AtomicU64,
  pub(crate) congestion_bbr: AtomicU64,
  pub(crate) congestion_cubic: AtomicU64,
}

impl Counters {
  pub(crate) fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
  }
}

struct Entry {
  alpn: Vec<u8>,
  identity: Option<Identity>,
  verified: bool,
  congestion: CongestionControl,
  tx: mpsc::Sender<Delivery>,
}

#[derive(Default)]
struct Table {
  entries: Vec<Entry>,
  /// The one client trust (roots, pins) of the routes that require verified clients.
  trust: Option<(Option<rustls::RootCertStore>, Vec<[u8; 32]>)>,
}

impl Table {
  fn mode(&self) -> ClientAuthMode {
    let verified = self.entries.iter().any(|e| e.verified);
    let open = self.entries.iter().any(|e| !e.verified);
    match (verified, open) {
      (false, _) => ClientAuthMode::None,
      (true, false) => ClientAuthMode::Required,
      (true, true) => ClientAuthMode::Optional,
    }
  }

  fn tls(&self, default: &Identity) -> Result<rustls::ServerConfig, QuicError> {
    let auth = match self.mode() {
      ClientAuthMode::Required => Some(ClientAuth::Required),
      ClientAuthMode::Optional => Some(ClientAuth::Optional),
      ClientAuthMode::None => None,
    };
    let verifier = match (auth, &self.trust) {
      (Some(auth), Some((roots, pinned))) => {
        tls::client_cert_verifier(roots.as_ref(), pinned, auth)?
      }
      _ => None,
    };
    let resolver = AlpnResolver {
      default: default.clone(),
      routes: self
        .entries
        .iter()
        .map(|e| (e.alpn.clone(), e.identity.clone()))
        .collect(),
    };
    let mut config = tls::server_tls_config_with(Arc::new(resolver), verifier)?;
    config.alpn_protocols = self.entries.iter().map(|e| e.alpn.clone()).collect();
    config.max_early_data_size = 0;
    Ok(config)
  }
}

/// The routing table of a [`QuicEndpoint`](super::QuicEndpoint): which ALPNs it negotiates,
/// which certificate each presents, and the client-certificate rule they imply.
pub struct AlpnRouter {
  table: Mutex<Table>,
  backlog: usize,
}

impl std::fmt::Debug for AlpnRouter {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AlpnRouter")
      .field(
        "alpns",
        &self
          .alpns()
          .iter()
          .map(|a| alpn_text(a))
          .collect::<Vec<_>>(),
      )
      .field("client_auth_mode", &self.client_auth_mode())
      .finish()
  }
}

impl AlpnRouter {
  pub(crate) fn new(backlog: usize) -> Self {
    Self {
      table: Mutex::new(Table::default()),
      backlog,
    }
  }

  fn table(&self) -> std::sync::MutexGuard<'_, Table> {
    self.table.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The registered ALPNs, in the server's order of preference.
  pub fn alpns(&self) -> Vec<Vec<u8>> {
    self
      .table()
      .entries
      .iter()
      .map(|e| e.alpn.clone())
      .collect()
  }

  /// How the handshake asks for client certificates.
  pub fn client_auth_mode(&self) -> ClientAuthMode {
    self.table().mode()
  }

  /// The congestion control of the route for `alpn`, if it is registered.
  pub fn congestion_control(&self, alpn: &[u8]) -> Option<CongestionControl> {
    self
      .table()
      .entries
      .iter()
      .find(|e| e.alpn == alpn)
      .map(|e| e.congestion)
  }

  /// The rustls server config for the current routes.
  pub(crate) fn tls(&self, default: &Identity) -> Result<rustls::ServerConfig, QuicError> {
    self.table().tls(default)
  }

  /// Add `route`, then `apply` the server config of the new table; on any failure the table
  /// is left as it was.
  pub(crate) fn add(
    &self,
    route: Route,
    default: &Identity,
    apply: impl FnOnce(rustls::ServerConfig) -> Result<(), QuicError>,
  ) -> Result<AlpnRoute, QuicError> {
    let mut table = self.table();
    let name = alpn_text(&route.alpn);
    if route.alpn.is_empty() {
      return Err(QuicError::Config("empty ALPN".into()));
    }
    if table.entries.iter().any(|e| e.alpn == route.alpn) {
      return Err(QuicError::AlpnTaken(name));
    }
    let trust = match route.policy {
      ClientCertPolicy::Ignore => None,
      ClientCertPolicy::RequireVerified { roots, pinned } => {
        if roots.is_none() && pinned.is_empty() {
          return Err(QuicError::NoClientTrust(name));
        }
        if table.trust.is_some() {
          return Err(QuicError::TrustConflict(name));
        }
        Some((roots, pinned))
      }
    };
    let (tx, rx) = mpsc::channel(self.backlog);
    let verified = trust.is_some();
    if verified {
      table.trust = trust;
    }
    table.entries.push(Entry {
      alpn: route.alpn.clone(),
      identity: route.identity,
      verified,
      congestion: route.congestion,
      tx,
    });
    if let Err(e) = table.tls(default).and_then(apply) {
      table.entries.pop();
      if verified {
        table.trust = None;
      }
      return Err(e);
    }
    Ok(AlpnRoute {
      alpn: route.alpn,
      rx: tokio::sync::Mutex::new(rx),
    })
  }

  /// Drop every route's sender, so their [`AlpnRoute::accept`] ends.
  pub(crate) fn close(&self) {
    self.table().entries.clear();
  }

  /// Tell the route of `alpn` (if any) about a refused attempt.
  pub(crate) fn refuse(&self, alpn: &[u8], refusal: Refusal) {
    if let Some(entry) = self.table().entries.iter().find(|e| e.alpn == alpn) {
      let _ = entry.tx.try_send(Err(refusal));
    }
  }

  /// Hand an established connection to its route, or close it: no route for its ALPN, no
  /// verified client chain where the route requires one (D7), or a full backlog.
  pub(crate) fn dispatch(&self, conn: quinn::Connection, counters: &Counters) {
    let table = self.table();
    let entry =
      negotiated_alpn(&conn).and_then(|alpn| table.entries.iter().find(|e| e.alpn == alpn));
    let Some(entry) = entry else {
      conn.close(CLOSE_NO_ROUTE.into(), b"no route for this ALPN");
      Counters::bump(&counters.refused_no_route);
      return;
    };
    if entry.verified && !has_verified_client_chain(&conn) {
      // Before the handler sees the connection: nothing of it is ever read.
      conn.close(
        CLOSE_CLIENT_CERT_REQUIRED.into(),
        b"a verified client certificate is required",
      );
      Counters::bump(&counters.refused_client_cert);
      let _ = entry.tx.try_send(Err(Refusal::ClientCertRequired {
        remote: conn.remote_address(),
      }));
      return;
    }
    match entry.tx.try_send(Ok(conn)) {
      Ok(()) => Counters::bump(&counters.accepted),
      Err(e) => {
        let full = matches!(e, mpsc::error::TrySendError::Full(_));
        if let Ok(conn) = e.into_inner() {
          if full {
            conn.close(CLOSE_BUSY.into(), b"busy");
            Counters::bump(&counters.refused_busy);
          } else {
            conn.close(CLOSE_NO_ROUTE.into(), b"route closed");
            Counters::bump(&counters.refused_no_route);
          }
        }
      }
    }
  }
}

/// The ALPN `conn` negotiated.
pub fn negotiated_alpn(conn: &quinn::Connection) -> Option<Vec<u8>> {
  conn
    .handshake_data()?
    .downcast::<quinn::crypto::rustls::HandshakeData>()
    .ok()?
    .protocol
}

/// Whether the peer of `conn` presented a client chain that the handshake verified. rustls
/// records a client chain only after its verifier accepted it, so a chain here was verified
/// against the endpoint's client trust.
pub fn has_verified_client_chain(conn: &quinn::Connection) -> bool {
  conn
    .peer_identity()
    .and_then(|identity| identity.downcast::<Vec<CertificateDer<'static>>>().ok())
    .is_some_and(|chain| !chain.is_empty())
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::Ordering;

  use super::*;
  use crate::quic::{QuicEndpoint, QuicEndpointConfig, QuicLimits, client_config};
  use crate::tls::{NodeCertificate, client_tls_config};

  /// A connection whose ALPN the router does not (or no longer) know is closed.
  #[tokio::test]
  async fn a_connection_without_a_route_is_closed() {
    let cert = NodeCertificate::self_signed("localhost").expect("cert");
    let identity = Identity::from_certificate(&cert).expect("identity");
    let endpoint = QuicEndpoint::bind(
      "127.0.0.1:0".parse().expect("addr"),
      QuicEndpointConfig::new(identity.clone()),
    )
    .expect("bind");
    let h3 = endpoint.register(Route::h3()).expect("h3");
    let tls = client_tls_config(&identity, None, &[cert.cert_sha256()]).expect("tls");
    let config = client_config(tls, &[ALPN_H3], &QuicLimits::default()).expect("config");
    let client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client");
    let conn = client
      .connect_with(config, endpoint.local_addr(), "localhost")
      .expect("connect")
      .await
      .expect("handshake");
    let served = h3.accept().await.expect("running").expect("delivered");

    let (empty, counters) = (AlpnRouter::new(1), Counters::default());
    empty.dispatch(served, &counters);
    assert_eq!(counters.refused_no_route.load(Ordering::Relaxed), 1);
    match conn.closed().await {
      quinn::ConnectionError::ApplicationClosed(close) => {
        assert_eq!(close.error_code.into_inner(), u64::from(CLOSE_NO_ROUTE));
      }
      other => panic!("unexpected close {other}"),
    }
    // Refusals for an unknown ALPN go nowhere.
    empty.refuse(
      ALPN_H3,
      Refusal::ClientCertRequired {
        remote: endpoint.local_addr(),
      },
    );
  }
}

/// Presents the certificate of the first route (in server order) the client offers.
#[derive(Debug)]
struct AlpnResolver {
  default: Identity,
  routes: Vec<(Vec<u8>, Option<Identity>)>,
}

impl rustls::server::ResolvesServerCert for AlpnResolver {
  fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
    let offered: Vec<&[u8]> = hello.alpn().map(Iterator::collect).unwrap_or_default();
    let identity = self
      .routes
      .iter()
      .find(|(alpn, _)| offered.contains(&alpn.as_slice()))
      .and_then(|(_, identity)| identity.as_ref())
      .unwrap_or(&self.default);
    Some(identity.current())
  }
}
