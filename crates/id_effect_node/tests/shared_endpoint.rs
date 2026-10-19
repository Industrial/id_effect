//! The mesh on the QUIC endpoint it shares with HTTP/3 (ADR 0014 D6/D7): one UDP port routed
//! by ALPN, `idm/1` refusing every connection without a verified client chain before a frame
//! of it is read, and `quic.mesh_port` splitting the mesh onto its own endpoint with
//! handshake-level mutual TLS.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use id_effect_node::transport::quic::{ALPN_MESH, QuicConfig, QuicSettings, QuicTransport};
use id_effect_node::transport::{Connection, Listener, Transport, TransportError};
use id_effect_platform::quic::{
  ALPN_H3, AlpnRoute, CLOSE_CLIENT_CERT_REQUIRED, ClientAuthMode, QuicEndpoint, QuicEndpointConfig,
  QuicLimits, Route, client_config, has_verified_client_chain, negotiated_alpn,
};
use id_effect_platform::tls::{Identity, NodeCertificate, ca_bundle_from_pem, client_tls_config};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(5);

/// A throwaway certificate authority.
struct Ca {
  cert: rcgen::Certificate,
  key: rcgen::KeyPair,
}

impl Ca {
  fn new(name: &str) -> Self {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, name);
    params.key_usages = vec![
      rcgen::KeyUsagePurpose::KeyCertSign,
      rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().expect("ca key");
    let cert = params.self_signed(&key).expect("ca cert");
    Self { cert, key }
  }

  fn roots(&self) -> rustls::RootCertStore {
    ca_bundle_from_pem(self.cert.pem().as_bytes()).expect("roots")
  }

  /// A leaf for `name`, valid for server and client auth.
  fn issue(&self, name: &str) -> NodeCertificate {
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).expect("params");
    params.extended_key_usages = vec![
      rcgen::ExtendedKeyUsagePurpose::ServerAuth,
      rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = params.signed_by(&key, &self.cert, &self.key).expect("leaf");
    NodeCertificate::from_pem(
      format!("{}{}", cert.pem(), self.cert.pem()).as_bytes(),
      key.serialize_pem().as_bytes(),
    )
    .expect("pem")
  }
}

/// A mesh node with a `"cluster"` certificate from `issuer`, trusting `trust`.
fn node(issuer: &Ca, trust: &Ca) -> QuicConfig {
  QuicConfig::from_certificate(&issuer.issue("cluster"))
    .expect("usable key")
    .with_client_verification(trust.roots())
}

/// An endpoint presenting the public ("localhost") certificate and serving HTTP/3.
fn shared_with(ca: &Ca, limits: QuicLimits) -> (QuicEndpoint, AlpnRoute) {
  let identity = Identity::from_certificate(&ca.issue("localhost")).expect("identity");
  let endpoint = QuicEndpoint::bind(
    "127.0.0.1:0".parse().expect("addr"),
    QuicEndpointConfig::new(identity).with_limits(limits),
  )
  .expect("bind");
  let h3 = endpoint.register(Route::h3()).expect("h3");
  (endpoint, h3)
}

fn shared(ca: &Ca) -> (QuicEndpoint, AlpnRoute) {
  shared_with(ca, QuicLimits::default())
}

/// A plain quinn client trusting `ca`, presenting `cert` when given, offering `alpn`.
async fn raw_dial(
  ca: &Ca,
  cert: Option<&NodeCertificate>,
  alpn: &[u8],
  addr: SocketAddr,
  name: &str,
) -> Result<quinn::Connection, String> {
  let tls = match cert {
    Some(cert) => client_tls_config(
      &Identity::from_certificate(cert).expect("identity"),
      Some(&ca.roots()),
      &[],
    )
    .expect("tls"),
    None => rustls::ClientConfig::builder_with_provider(id_effect_platform::tls::provider())
      .with_protocol_versions(&[&rustls::version::TLS13])
      .expect("tls13")
      .with_root_certificates(ca.roots())
      .with_no_client_auth(),
  };
  let config = client_config(tls, &[alpn], &QuicLimits::default()).expect("config");
  let client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client");
  tokio::time::timeout(
    WAIT,
    client
      .connect_with(config, addr, name)
      .map_err(|e| e.to_string())?,
  )
  .await
  .map_err(|_| "timed out".to_string())?
  .map_err(|e| e.to_string())
}

/// Echo one frame per accepted connection; every accept result goes to the receiver, with
/// the first frame read from accepted connections (`Err` = refused).
fn echo(listener: Box<dyn Listener>) -> mpsc::UnboundedReceiver<Result<Bytes, String>> {
  let (tx, rx) = mpsc::unbounded_channel();
  tokio::spawn(async move {
    loop {
      match listener.accept().await {
        Ok(conn) => {
          let tx = tx.clone();
          tokio::spawn(async move {
            if let Ok(frame) = conn.recv().await {
              let _ = tx.send(Ok(frame.clone()));
              let _ = conn.send(frame).await;
            }
            while conn.recv().await.is_ok() {}
          });
        }
        Err(TransportError::Closed) => break,
        Err(e) => {
          let _ = tx.send(Err(e.to_string()));
        }
      }
    }
  });
  rx
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Result<Bytes, String>>) -> Result<Bytes, String> {
  tokio::time::timeout(WAIT, rx.recv())
    .await
    .expect("listener saw the attempt")
    .expect("listener running")
}

/// Dial with `config` and check one frame comes back.
async fn round_trip(transport: &QuicTransport, addr: &str) -> Result<Box<dyn Connection>, String> {
  tokio::time::timeout(WAIT, async {
    let conn = transport.connect(addr).await.map_err(|e| e.to_string())?;
    conn
      .send(Bytes::from_static(b"ping"))
      .await
      .map_err(|e| e.to_string())?;
    let echo = conn.recv().await.map_err(|e| e.to_string())?;
    assert_eq!(echo, Bytes::from_static(b"ping"));
    Ok(conn)
  })
  .await
  .map_err(|_| "timed out".to_string())?
}

#[tokio::test]
async fn alpn_routes_h3_and_idm() {
  let ca = Ca::new("mesh ca");
  let (endpoint, h3) = shared(&ca);
  let listener = QuicTransport::on_endpoint(node(&ca, &ca), &endpoint)
    .listen("the endpoint is already bound")
    .await
    .expect("idm/1 registered");
  let addr = endpoint.local_addr();
  assert_eq!(listener.local_addr(), addr.to_string(), "one UDP port");
  assert_eq!(
    endpoint.router().alpns(),
    vec![ALPN_H3.to_vec(), ALPN_MESH.to_vec()]
  );
  assert_eq!(
    endpoint.router().client_auth_mode(),
    ClientAuthMode::Optional
  );
  let mut accepted = echo(listener);

  // HTTP/3 without a client certificate, verifying the public name.
  let _browser = raw_dial(&ca, None, ALPN_H3, addr, "localhost")
    .await
    .expect("h3 handshake");
  let served = tokio::time::timeout(WAIT, h3.accept())
    .await
    .expect("routed")
    .expect("running")
    .expect("h3 connection");
  assert_eq!(negotiated_alpn(&served).as_deref(), Some(ALPN_H3));
  assert!(!has_verified_client_chain(&served));

  // The mesh on the same port, verifying the cluster name (the idm/1 certificate).
  let dialer = QuicTransport::try_new(node(&ca, &ca)).expect("trusted");
  let _conn = round_trip(&dialer, &addr.to_string())
    .await
    .expect("mesh round trip");
  assert_eq!(next(&mut accepted).await, Ok(Bytes::from_static(b"ping")));

  // A second mesh listener on the same endpoint is refused.
  let again = QuicTransport::on_endpoint(node(&ca, &ca), &endpoint)
    .listen("")
    .await
    .err()
    .expect("idm/1 taken");
  assert!(
    matches!(&again, TransportError::Bind(m) if m.contains("already routed")),
    "{again}"
  );

  // A node on another shared endpoint dials from that endpoint's port.
  let (other, _other_h3) = shared(&ca);
  let from_shared = QuicTransport::on_endpoint(node(&ca, &ca), &other);
  let conn = round_trip(&from_shared, &addr.to_string())
    .await
    .expect("dial from a shared endpoint");
  assert_eq!(next(&mut accepted).await, Ok(Bytes::from_static(b"ping")));
  drop(conn);
  assert_eq!(endpoint.stats().accepted, 3);
}

/// D7, the negative test: a mesh peer without a client certificate completes the (optional)
/// handshake on the shared port, opens a stream and sends a frame — and the connection is
/// closed with `CLOSE_CLIENT_CERT_REQUIRED` before the mesh reads anything: the listener
/// reports a refusal and never yields the connection.
#[tokio::test]
async fn idm_without_client_cert_refused_on_shared_endpoint() {
  let ca = Ca::new("mesh ca");
  let (endpoint, _h3) = shared(&ca);
  let listener = QuicTransport::on_endpoint(node(&ca, &ca), &endpoint)
    .listen("")
    .await
    .expect("listen");
  let addr = endpoint.local_addr();
  let mut accepted = echo(listener);

  let conn = raw_dial(&ca, None, ALPN_MESH, addr, "cluster")
    .await
    .expect("the handshake completes: client certificates are optional on the shared port");
  let (mut send, mut recv) = conn.open_bi().await.expect("stream");
  let _ = send.write_all(&[0, 0, 0, 4, b'p', b'i', b'n', b'g']).await;
  let reason = tokio::time::timeout(WAIT, conn.closed())
    .await
    .expect("closed promptly");
  assert!(
    matches!(
      &reason,
      quinn::ConnectionError::ApplicationClosed(close)
        if close.error_code.into_inner() == u64::from(CLOSE_CLIENT_CERT_REQUIRED)
    ),
    "{reason}"
  );
  let mut echo_buf = [0u8; 8];
  assert!(recv.read_exact(&mut echo_buf).await.is_err(), "no echo");

  let verdict = next(&mut accepted).await;
  assert!(
    matches!(&verdict, Err(m) if m.contains("no verified client certificate")),
    "{verdict:?}"
  );
  let stats = endpoint.stats();
  assert_eq!(stats.refused_client_cert, 1);
  assert_eq!(stats.accepted, 0, "the connection never reached the mesh");

  // The same listener still serves a node with a certificate.
  let dialer = QuicTransport::try_new(node(&ca, &ca)).expect("trusted");
  round_trip(&dialer, &addr.to_string())
    .await
    .expect("certified peer");
  assert_eq!(next(&mut accepted).await, Ok(Bytes::from_static(b"ping")));
}

#[tokio::test]
async fn idm_cert_from_other_ca_refused_on_shared_endpoint() {
  let (ca, other) = (Ca::new("mesh ca"), Ca::new("other ca"));
  let (endpoint, _h3) = shared(&ca);
  let listener = QuicTransport::on_endpoint(node(&ca, &ca), &endpoint)
    .listen("")
    .await
    .expect("listen");
  let addr = endpoint.local_addr().to_string();
  let mut accepted = echo(listener);

  // It trusts the listener; only the client certificate can fail, during the handshake.
  let stranger = QuicTransport::try_new(node(&other, &ca)).expect("trusted");
  assert!(round_trip(&stranger, &addr).await.is_err());
  let verdict = next(&mut accepted).await;
  assert!(
    matches!(&verdict, Err(m) if m.starts_with("connect failed: quic handshake with")),
    "{verdict:?}"
  );
  assert_eq!(endpoint.stats().accepted, 0);
  assert!(endpoint.stats().handshakes_failed >= 1);
}

#[tokio::test]
async fn mesh_port_splits_endpoint() {
  let parsed: QuicSettings = serde_json::from_str(r#"{ "mesh_port": 4370 }"#).expect("parse");
  assert_eq!(parsed.mesh_port, Some(4370));
  assert_eq!(
    serde_json::from_str::<QuicSettings>("{}").expect("parse"),
    QuicSettings::default()
  );
  assert!(serde_json::from_str::<QuicSettings>(r#"{ "mesh_prot": 1 }"#).is_err());

  let ca = Ca::new("mesh ca");
  let (endpoint, _h3) = shared(&ca);
  let split = QuicSettings { mesh_port: Some(0) };
  let (transport, listen_on) = split.mesh_transport(node(&ca, &ca), &endpoint);
  let listener = transport.listen(&listen_on).await.expect("own endpoint");
  let mesh_addr = listener.local_addr();
  assert_ne!(mesh_addr, endpoint.local_addr().to_string(), "its own port");
  assert!(mesh_addr.starts_with("127.0.0.1:"));
  assert_eq!(endpoint.router().alpns(), vec![ALPN_H3.to_vec()]);
  let mut accepted = echo(listener);

  let dialer = QuicTransport::try_new(node(&ca, &ca)).expect("trusted");
  assert!(
    round_trip(&dialer, &endpoint.local_addr().to_string())
      .await
      .is_err(),
    "the shared port no longer speaks idm/1"
  );
  round_trip(&dialer, &mesh_addr)
    .await
    .expect("mesh on its own port");
  assert_eq!(next(&mut accepted).await, Ok(Bytes::from_static(b"ping")));

  // Handshake-level mutual TLS there: no application close code, a TLS failure.
  let refused = match raw_dial(
    &ca,
    None,
    ALPN_MESH,
    mesh_addr.parse().expect("addr"),
    "cluster",
  )
  .await
  {
    Ok(conn) => Some(
      tokio::time::timeout(WAIT, conn.closed())
        .await
        .expect("closed"),
    ),
    Err(_) => None,
  };
  assert!(
    !matches!(refused, Some(quinn::ConnectionError::ApplicationClosed(_))),
    "{refused:?}"
  );
  assert!(next(&mut accepted).await.is_err());

  // Without mesh_port the mesh joins the shared endpoint.
  let (transport, listen_on) = QuicSettings::default().mesh_transport(node(&ca, &ca), &endpoint);
  assert_eq!(listen_on, endpoint.local_addr().to_string());
  let _listener = transport.listen(&listen_on).await.expect("shared");
  assert_eq!(
    endpoint.router().alpns(),
    vec![ALPN_H3.to_vec(), ALPN_MESH.to_vec()]
  );
}

#[test]
fn deprecated_node_certs_path_still_compiles() {
  #[allow(deprecated)]
  use id_effect_node::transport::certs::{
    CertError, CertReloader, NodeCertificate as OldPath, ca_bundle_from_pem as old_bundle,
  };
  let cert = OldPath::self_signed("node").expect("cert");
  // The same type as the new home.
  let moved: NodeCertificate = cert;
  assert_eq!(old_bundle(b"").err(), Some(CertError::NoCertificate));
  let _: fn(&str, &str) -> Result<CertReloader, CertError> = |c, k| CertReloader::new(c, k);
  QuicConfig::from_certificate(&moved).expect("usable");
}

#[tokio::test]
async fn no_client_auth_mesh_takes_anonymous_peers_on_shared_endpoint() {
  let ca = Ca::new("mesh ca");
  let (endpoint, _h3) = shared(&ca);
  let config = QuicConfig::from_certificate(&ca.issue("cluster"))
    .expect("usable")
    .dangerous_no_client_auth();
  let listener = QuicTransport::on_endpoint(config, &endpoint)
    .listen("")
    .await
    .expect("listen");
  assert_eq!(endpoint.router().client_auth_mode(), ClientAuthMode::None);
  let mut accepted = echo(listener);
  let conn = raw_dial(&ca, None, ALPN_MESH, endpoint.local_addr(), "cluster")
    .await
    .expect("anonymous");
  let (mut send, _recv) = conn.open_bi().await.expect("stream");
  send
    .write_all(&[0, 0, 0, 2, b'h', b'i'])
    .await
    .expect("write");
  assert_eq!(next(&mut accepted).await, Ok(Bytes::from_static(b"hi")));
}

#[tokio::test]
async fn listener_bounds_the_wait_for_the_mesh_stream_and_ends_with_the_endpoint() {
  let ca = Ca::new("mesh ca");
  let mut limits = QuicLimits::default();
  limits.handshake_timeout = Duration::from_millis(300);
  let (endpoint, _h3) = shared_with(&ca, limits);
  let listener = QuicTransport::on_endpoint(node(&ca, &ca), &endpoint)
    .listen("")
    .await
    .expect("listen");
  let addr = endpoint.local_addr();
  let peer = ca.issue("cluster");

  // A verified peer that never opens its stream.
  let silent = raw_dial(&ca, Some(&peer), ALPN_MESH, addr, "cluster")
    .await
    .expect("handshake");
  let err = listener.accept().await.err().expect("no stream");
  assert!(err.to_string().contains("opened no stream"), "{err}");
  drop(silent);

  // A verified peer that leaves before opening it.
  let gone = raw_dial(&ca, Some(&peer), ALPN_MESH, addr, "cluster")
    .await
    .expect("handshake");
  gone.close(0u32.into(), b"bye");
  assert!(matches!(
    listener.accept().await,
    Err(TransportError::Connect(_))
  ));

  endpoint.close();
  assert!(matches!(
    listener.accept().await,
    Err(TransportError::Closed)
  ));
}

#[tokio::test]
async fn dial_errors_and_peer_addresses() {
  let ca = Ca::new("mesh ca");
  let (endpoint, _h3) = shared(&ca);
  // Clients verified against their own CA roots.
  let server = QuicConfig::from_certificate(&ca.issue("cluster"))
    .expect("usable")
    .with_client_cert_roots(ca.roots());
  let listener = QuicTransport::on_endpoint(server, &endpoint)
    .listen("")
    .await
    .expect("listen");
  let addr = endpoint.local_addr().to_string();
  let mut accepted = echo(listener);

  let dialer = QuicTransport::try_new(node(&ca, &ca)).expect("trusted");
  let conn = round_trip(&dialer, &addr).await.expect("round trip");
  assert_eq!(conn.peer_addr(), addr);
  assert_eq!(next(&mut accepted).await, Ok(Bytes::from_static(b"ping")));

  assert!(matches!(
    dialer.connect("nowhere").await.err(),
    Some(TransportError::Connect(m)) if m.contains("bad addr")
  ));
  // A server name the mesh certificate does not carry, from a shared endpoint.
  let (other, _other_h3) = shared(&ca);
  let wrong_name = QuicTransport::on_endpoint(node(&ca, &ca).with_server_name("elsewhere"), &other);
  assert!(matches!(
    wrong_name.connect(&addr).await.err(),
    Some(TransportError::Connect(m)) if m.starts_with("quic connect")
  ));
  // A server name that is not one at all, from an own socket.
  let no_name = QuicTransport::try_new(node(&ca, &ca).with_server_name("")).expect("trusted");
  assert!(matches!(
    no_name.connect(&addr).await.err(),
    Some(TransportError::Connect(_))
  ));
}

#[tokio::test]
async fn own_endpoint_listen_needs_a_valid_address() {
  let ca = Ca::new("mesh ca");
  let err = QuicTransport::new(node(&ca, &ca))
    .listen("not an address")
    .await
    .err()
    .expect("bad address");
  assert!(
    matches!(&err, TransportError::Bind(m) if m.contains("bad addr")),
    "{err}"
  );
}
