//! The shared QUIC endpoint (ADR 0014 D6/D7, ADR 0013 R4/R8): ALPN routing, the
//! client-certificate rule, Retry, finite limits, no 0-RTT, and the refusals.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_platform::quic::{
  ALPN_H3, ALPN_MESH, AlpnRoute, CLOSE_BUSY, CLOSE_CLIENT_CERT_REQUIRED, CLOSE_NO_ROUTE,
  ClientAuthMode, DANGEROUS_NO_RETRY, EndpointStats, QuicEndpoint, QuicEndpointConfig, QuicError,
  QuicLimits, Refusal, Route, client_config, has_verified_client_chain, negotiated_alpn,
};
use id_effect_platform::tls::{Identity, NodeCertificate, ca_bundle_from_pem, client_tls_config};

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

  fn identity(&self, name: &str) -> Identity {
    Identity::from_certificate(&self.issue(name)).expect("identity")
  }
}

/// An endpoint presenting a "localhost" certificate from `ca`, with `limits`.
fn endpoint_with(ca: &Ca, limits: QuicLimits) -> QuicEndpoint {
  QuicEndpoint::bind(
    "127.0.0.1:0".parse().expect("addr"),
    QuicEndpointConfig::new(ca.identity("localhost")).with_limits(limits),
  )
  .expect("bind")
}

fn endpoint(ca: &Ca) -> QuicEndpoint {
  endpoint_with(ca, QuicLimits::default())
}

/// rustls client config trusting `ca`, presenting `cert` when given.
fn tls(ca: &Ca, cert: Option<&Identity>) -> rustls::ClientConfig {
  match cert {
    Some(identity) => client_tls_config(identity, Some(&ca.roots()), &[]).expect("tls"),
    None => rustls::ClientConfig::builder_with_provider(id_effect_platform::tls::provider())
      .with_protocol_versions(&[&rustls::version::TLS13])
      .expect("tls13")
      .with_root_certificates(ca.roots())
      .with_no_client_auth(),
  }
}

/// Dial `addr` for `name` offering `alpns` from a fresh client socket.
async fn dial(
  tls: rustls::ClientConfig,
  alpns: &[&[u8]],
  addr: SocketAddr,
  name: &str,
) -> Result<quinn::Connection, String> {
  let config = client_config(tls, alpns, &QuicLimits::default()).expect("client config");
  let client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client");
  let connecting = client
    .connect_with(config, addr, name)
    .map_err(|e| e.to_string())?;
  tokio::time::timeout(WAIT, connecting)
    .await
    .map_err(|_| "timed out".to_string())?
    .map_err(|e| e.to_string())
}

/// The next delivery on `route`.
async fn next(route: &AlpnRoute) -> Result<quinn::Connection, Refusal> {
  tokio::time::timeout(WAIT, route.accept())
    .await
    .expect("route saw the attempt")
    .expect("endpoint running")
}

/// The application close code of a connection the server closed.
async fn closed_with(conn: &quinn::Connection) -> Option<u64> {
  match tokio::time::timeout(WAIT, conn.closed())
    .await
    .expect("closed")
  {
    quinn::ConnectionError::ApplicationClosed(close) => Some(close.error_code.into_inner()),
    _ => None,
  }
}

/// Poll `stats` until `done` holds.
async fn until(endpoint: &QuicEndpoint, done: impl Fn(&EndpointStats) -> bool) -> EndpointStats {
  tokio::time::timeout(WAIT, async {
    loop {
      let stats = endpoint.stats();
      if done(&stats) {
        return stats;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("counter reached")
}

#[tokio::test]
async fn h3_route_takes_clients_with_and_without_certificates() {
  let ca = Ca::new("ca");
  let endpoint = endpoint(&ca);
  let h3 = endpoint.register(Route::h3()).expect("h3");
  assert_eq!(h3.alpn(), ALPN_H3);
  assert_eq!(endpoint.router().client_auth_mode(), ClientAuthMode::None);
  let addr = endpoint.local_addr();

  let conn = dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
    .await
    .expect("anonymous h3");
  let served = next(&h3).await.expect("delivered");
  assert_eq!(negotiated_alpn(&served).as_deref(), Some(ALPN_H3));
  assert!(!has_verified_client_chain(&served));
  drop(conn);

  // With a mesh route beside it the handshake asks (optionally) for a certificate;
  // h3 still takes clients without one.
  let _mesh = endpoint
    .register(Route::mesh(Some(ca.roots()), vec![]).with_identity(ca.identity("cluster")))
    .expect("mesh");
  assert_eq!(
    endpoint.router().client_auth_mode(),
    ClientAuthMode::Optional
  );
  assert_eq!(
    endpoint.router().alpns(),
    vec![ALPN_H3.to_vec(), ALPN_MESH.to_vec()]
  );
  let _anonymous = dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
    .await
    .expect("anonymous h3 on the shared endpoint");
  assert!(!has_verified_client_chain(
    &next(&h3).await.expect("delivered")
  ));
  let client = ca.identity("client");
  let _certified = dial(tls(&ca, Some(&client)), &[ALPN_H3], addr, "localhost")
    .await
    .expect("certified h3");
  assert!(has_verified_client_chain(
    &next(&h3).await.expect("delivered")
  ));
  assert!(format!("{:?}", endpoint.router()).contains("Optional"));
}

/// D7 at the endpoint: on the shared shape a route that requires a verified client closes a
/// connection without one before its handler sees it; a certificate from another CA fails the
/// handshake; each route presents its own certificate.
#[tokio::test]
async fn required_route_refuses_unverified_clients_before_the_handler() {
  let (ca, other) = (Ca::new("ca"), Ca::new("other"));
  let endpoint = endpoint(&ca);
  let _h3 = endpoint.register(Route::h3()).expect("h3");
  let mesh = endpoint
    .register(Route::mesh(Some(ca.roots()), vec![]).with_identity(ca.identity("cluster")))
    .expect("mesh");
  let addr = endpoint.local_addr();

  // No certificate: the handshake completes (optional), then the route closes it.
  let conn = dial(tls(&ca, None), &[ALPN_MESH], addr, "cluster")
    .await
    .expect("handshake completes without a certificate");
  let (mut send, _recv) = conn.open_bi().await.expect("stream");
  let _ = send.write_all(&[0, 0, 0, 4, b'p', b'i', b'n', b'g']).await;
  assert_eq!(
    closed_with(&conn).await,
    Some(u64::from(CLOSE_CLIENT_CERT_REQUIRED))
  );
  let refused = next(&mesh).await.expect_err("refused, not delivered");
  assert!(matches!(refused, Refusal::ClientCertRequired { .. }));
  assert!(
    refused.to_string().contains("refused before any frame"),
    "{refused}"
  );
  let stats = endpoint.stats();
  assert_eq!((stats.refused_client_cert, stats.accepted), (1, 0));

  // A certificate from another CA fails the handshake.
  // (The client may see its half complete before the server's alert arrives.)
  let stranger = other.identity("cluster");
  let _ = dial(tls(&ca, Some(&stranger)), &[ALPN_MESH], addr, "cluster").await;
  let refused = next(&mesh).await;
  assert!(
    matches!(&refused, Err(Refusal::Handshake { .. })),
    "{refused:?}"
  );
  assert!(
    refused
      .unwrap_err()
      .to_string()
      .starts_with("quic handshake with")
  );

  // A cluster certificate gets through; the dialer verified the mesh certificate for
  // "cluster" (the h3 route presents "localhost").
  let node = ca.identity("cluster");
  let conn = dial(tls(&ca, Some(&node)), &[ALPN_MESH], addr, "cluster")
    .await
    .expect("verified client");
  let served = next(&mesh).await.expect("delivered");
  assert!(has_verified_client_chain(&served));
  assert_eq!(
    dial(tls(&ca, None), &[ALPN_H3], addr, "cluster")
      .await
      .map(|_| ())
      .map_err(|e| e.contains("certificate")),
    Err(true),
    "h3 presents the endpoint certificate, not the mesh one"
  );
  drop(conn);
}

#[tokio::test]
async fn mesh_only_endpoint_requires_certificates_in_the_handshake() {
  let ca = Ca::new("ca");
  let endpoint = QuicEndpoint::bind(
    "127.0.0.1:0".parse().expect("addr"),
    QuicEndpointConfig::new(ca.identity("cluster")),
  )
  .expect("bind");
  let mesh = endpoint
    .register(Route::mesh(Some(ca.roots()), vec![]))
    .expect("mesh");
  assert_eq!(
    endpoint.router().client_auth_mode(),
    ClientAuthMode::Required
  );
  let addr = endpoint.local_addr();
  // The server refuses in the handshake: a TLS alert, not the route's close code.
  let refused = match dial(tls(&ca, None), &[ALPN_MESH], addr, "cluster").await {
    Ok(conn) => closed_with(&conn).await,
    Err(_) => None,
  };
  assert_eq!(refused, None);
  assert!(matches!(next(&mesh).await, Err(Refusal::Handshake { .. })));
  assert_eq!(endpoint.stats().refused_client_cert, 0);
}

#[tokio::test]
async fn unknown_alpn_refused() {
  let ca = Ca::new("ca");
  let endpoint = endpoint(&ca);
  let addr = endpoint.local_addr();
  // No route yet: nothing is negotiated.
  assert!(
    dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
      .await
      .is_err()
  );
  let _h3 = endpoint.register(Route::h3()).expect("h3");
  assert!(
    dial(tls(&ca, None), &[b"xyz/1"], addr, "localhost")
      .await
      .is_err()
  );
  assert!(
    dial(tls(&ca, None), &[], addr, "localhost").await.is_err(),
    "no ALPN"
  );
  until(&endpoint, |s| s.handshakes_failed >= 3).await;
  assert_eq!(endpoint.stats().accepted, 0);
}

#[tokio::test]
async fn dropped_route_and_full_backlog_close_with_their_codes() {
  let ca = Ca::new("ca");
  let mut limits = QuicLimits::default();
  limits.route_backlog = 1;
  let endpoint = endpoint_with(&ca, limits);
  let h3 = endpoint.register(Route::h3()).expect("h3");
  let addr = endpoint.local_addr();

  let first = dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
    .await
    .expect("first");
  until(&endpoint, |s| s.accepted == 1).await;
  let second = dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
    .await
    .expect("second");
  assert_eq!(closed_with(&second).await, Some(u64::from(CLOSE_BUSY)));
  assert_eq!(endpoint.stats().refused_busy, 1);
  drop((first, h3));

  let third = dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
    .await
    .expect("negotiated");
  assert_eq!(closed_with(&third).await, Some(u64::from(CLOSE_NO_ROUTE)));
  assert_eq!(endpoint.stats().refused_no_route, 1);
}

#[tokio::test]
async fn route_registration_errors_leave_the_table_unchanged() {
  let (ca, other) = (Ca::new("ca"), Ca::new("other"));
  let endpoint = endpoint(&ca);
  assert!(matches!(
    endpoint.register(Route::new(Vec::new())),
    Err(QuicError::Config(m)) if m.contains("empty ALPN")
  ));
  assert!(matches!(
    endpoint.register(Route::mesh(None, vec![])),
    Err(QuicError::NoClientTrust(alpn)) if alpn == "idm/1"
  ));
  // Roots that cannot verify anything: the config fails and the route is not added.
  let err = endpoint
    .register(Route::mesh(Some(rustls::RootCertStore::empty()), vec![]))
    .expect_err("unusable roots");
  assert!(err.to_string().starts_with("tls: client roots"), "{err}");
  assert!(endpoint.router().alpns().is_empty());

  let route = Route::mesh(Some(ca.roots()), vec![]);
  assert_eq!(route.alpn(), ALPN_MESH);
  assert!(matches!(
    route.policy(),
    id_effect_platform::quic::ClientCertPolicy::RequireVerified { .. }
  ));
  let _mesh = endpoint.register(route).expect("mesh");
  assert!(matches!(
    endpoint.register(Route::mesh(Some(other.roots()), vec![])),
    Err(QuicError::AlpnTaken(alpn)) if alpn == "idm/1"
  ));
  let err = endpoint
    .register(Route::new(b"idm/2".to_vec()).require_verified_client(Some(other.roots()), vec![]))
    .expect_err("second trust");
  assert!(matches!(err, QuicError::TrustConflict(_)), "{err}");
  assert_eq!(endpoint.router().alpns(), vec![ALPN_MESH.to_vec()]);
}

#[tokio::test]
async fn r4_quic_retry_on_by_default() {
  let ca = Ca::new("ca");
  assert!(QuicEndpointConfig::new(ca.identity("localhost")).retry());
  let endpoint = endpoint(&ca);
  assert!(endpoint.retry_enabled());
  let _h3 = endpoint.register(Route::h3()).expect("h3");
  let _conn = dial(
    tls(&ca, None),
    &[ALPN_H3],
    endpoint.local_addr(),
    "localhost",
  )
  .await
  .expect("validated");
  let stats = until(&endpoint, |s| s.accepted == 1).await;
  // At least one: a client that resends its Initial before the Retry lands gets a Retry for
  // that packet too (seen on a loaded macOS runner: 2).
  assert!(
    stats.retries_sent >= 1,
    "a Retry before the handshake: {}",
    stats.retries_sent
  );

  // The opt-out is recorded and skips the Retry.
  let config = QuicEndpointConfig::new(ca.identity("localhost")).dangerous_no_retry();
  assert!(!config.retry());
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == DANGEROUS_NO_RETRY)
  );
  let unvalidated = QuicEndpoint::bind("127.0.0.1:0".parse().expect("addr"), config).expect("bind");
  let _h3 = unvalidated.register(Route::h3()).expect("h3");
  let _conn = dial(
    tls(&ca, None),
    &[ALPN_H3],
    unvalidated.local_addr(),
    "localhost",
  )
  .await
  .expect("no retry");
  let stats = until(&unvalidated, |s| s.accepted == 1).await;
  assert_eq!(stats.retries_sent, 0);
}

#[tokio::test]
async fn r4_quic_limits_finite_by_default() {
  let limits = QuicLimits::default();
  assert!(limits.max_concurrent_bidi_streams > 0 && limits.max_concurrent_bidi_streams <= 100);
  assert!(limits.max_concurrent_uni_streams > 0 && limits.max_concurrent_uni_streams <= 100);
  assert!(limits.idle_timeout > Duration::ZERO && limits.idle_timeout <= Duration::from_secs(30));
  assert!(limits.max_udp_payload_size >= 1200 && limits.max_udp_payload_size <= 1472);
  assert!(limits.stream_receive_window <= limits.receive_window);
  assert!(limits.receive_window <= 10_000_000);
  assert!(limits.max_pending_handshakes <= 256 && limits.route_backlog <= 64);
  assert!(limits.handshake_timeout <= Duration::from_secs(10));

  let ca = Ca::new("ca");
  // Raising a limit needs the opt-out; zero is refused outright.
  let mut raised = QuicLimits::default();
  raised.max_concurrent_bidi_streams = 1_000;
  let config = QuicEndpointConfig::new(ca.identity("localhost")).with_limits(raised.clone());
  assert_eq!(
    QuicEndpoint::bind("127.0.0.1:0".parse().expect("addr"), config).err(),
    Some(QuicError::LimitAboveDefault("max_concurrent_bidi_streams"))
  );
  let config = QuicEndpointConfig::new(ca.identity("localhost"))
    .with_limits(raised)
    .dangerous_raise_limits();
  assert_eq!(config.limits().max_concurrent_bidi_streams, 1_000);
  QuicEndpoint::bind("127.0.0.1:0".parse().expect("addr"), config).expect("raised on purpose");

  // The stream credit is enforced: a peer cannot open a third stream with a limit of two.
  let mut tight = QuicLimits::default();
  tight.max_concurrent_bidi_streams = 2;
  let endpoint = endpoint_with(&ca, tight);
  assert_eq!(endpoint.limits().max_concurrent_bidi_streams, 2);
  let h3 = endpoint.register(Route::h3()).expect("h3");
  let conn = dial(
    tls(&ca, None),
    &[ALPN_H3],
    endpoint.local_addr(),
    "localhost",
  )
  .await
  .expect("dial");
  let _served = next(&h3).await.expect("delivered");
  let mut open = Vec::new();
  for _ in 0..2 {
    let (mut send, recv) = conn.open_bi().await.expect("within the credit");
    send.write_all(b"x").await.expect("write");
    open.push((send, recv));
  }
  assert!(
    tokio::time::timeout(Duration::from_millis(300), conn.open_bi())
      .await
      .is_err(),
    "a third stream waits for credit"
  );
}

/// 0-RTT is off: a resumed client cannot send early data.
#[tokio::test]
async fn zero_rtt_is_refused() {
  let ca = Ca::new("ca");
  let endpoint = endpoint(&ca);
  let h3 = endpoint.register(Route::h3()).expect("h3");
  let addr = endpoint.local_addr();
  // `client_config` never asks for early data; ask for it by hand.
  let mut early = tls(&ca, None);
  early.enable_early_data = true;
  early.alpn_protocols = vec![ALPN_H3.to_vec()];
  let config = quinn::ClientConfig::new(Arc::new(
    quinn::crypto::rustls::QuicClientConfig::try_from(early).expect("quic tls"),
  ));
  let client = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client");
  let first = client
    .connect_with(config.clone(), addr, "localhost")
    .expect("connect")
    .await
    .expect("first");
  let _ = next(&h3).await.expect("delivered");
  // Let the session ticket arrive, then try to resume with early data.
  tokio::time::sleep(Duration::from_millis(100)).await;
  drop(first);
  let second = client
    .connect_with(config, addr, "localhost")
    .expect("connect");
  assert!(second.into_0rtt().is_err(), "the server offers no 0-RTT");
}

#[tokio::test]
async fn endpoints_dial_each_other_from_their_socket() {
  let ca = Ca::new("ca");
  let (a, b) = (endpoint(&ca), endpoint(&ca));
  let served = b.register(Route::h3()).expect("h3");
  let config = client_config(tls(&ca, None), &[ALPN_H3], a.limits()).expect("config");
  let conn = a
    .connect(b.local_addr(), "localhost", config.clone())
    .await
    .expect("dial");
  let accepted = next(&served).await.expect("delivered");
  assert_eq!(
    accepted.remote_address(),
    a.local_addr(),
    "dialed from A's port"
  );
  drop(conn);
  assert!(matches!(
    a.connect(b.local_addr(), "", config.clone()).await,
    Err(QuicError::Connect(_))
  ));
  assert!(matches!(
    a.connect(b.local_addr(), "elsewhere", config).await,
    Err(QuicError::Connect(m)) if m.contains("certificate")
  ));

  // Closing ends the routes.
  b.close();
  assert!(served.accept().await.is_none());
}

#[test]
fn bind_needs_a_runtime_and_a_free_port() {
  let ca = Ca::new("ca");
  let err = QuicEndpoint::bind(
    "127.0.0.1:0".parse().expect("addr"),
    QuicEndpointConfig::new(ca.identity("localhost")),
  )
  .expect_err("no runtime");
  assert_eq!(err, QuicError::Bind("no async runtime".into()));

  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("runtime");
  runtime.block_on(async {
    let taken = endpoint(&ca);
    let err = QuicEndpoint::bind(
      taken.local_addr(),
      QuicEndpointConfig::new(ca.identity("localhost")),
    )
    .expect_err("port in use");
    assert!(matches!(err, QuicError::Bind(_)), "{err}");
    let mut zero = QuicLimits::default();
    zero.handshake_timeout = Duration::ZERO;
    assert!(matches!(
      QuicEndpoint::bind(
        "127.0.0.1:0".parse().expect("addr"),
        QuicEndpointConfig::new(ca.identity("localhost")).with_limits(zero)
      ),
      Err(QuicError::Config(_))
    ));
    let mut huge = QuicLimits::default();
    huge.max_udp_payload_size = u16::MAX;
    assert!(matches!(
      QuicEndpoint::bind(
        "127.0.0.1:0".parse().expect("addr"),
        QuicEndpointConfig::new(ca.identity("localhost"))
          .with_limits(huge)
          .dangerous_raise_limits()
      ),
      Err(QuicError::Config(m)) if m.starts_with("max_udp_payload_size")
    ));
  });
}

/// A server-certificate check that waits until released, so the server's half of the handshake
/// stays pending.
#[derive(Debug)]
struct Stall {
  release: Mutex<std::sync::mpsc::Receiver<()>>,
  provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Stall {
  fn verify_server_cert(
    &self,
    _end_entity: &rustls::pki_types::CertificateDer<'_>,
    _intermediates: &[rustls::pki_types::CertificateDer<'_>],
    _server_name: &rustls::pki_types::ServerName<'_>,
    _ocsp: &[u8],
    _now: rustls::pki_types::UnixTime,
  ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
    let _ = self
      .release
      .lock()
      .expect("lock")
      .recv_timeout(Duration::from_secs(10));
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
      &self.provider.signature_verification_algorithms,
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
      &self.provider.signature_verification_algorithms,
    )
  }

  fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
    self
      .provider
      .signature_verification_algorithms
      .supported_schemes()
  }
}

/// Pre-auth limits: one pending handshake at most, and a handshake that does not finish in
/// time is dropped and reported to its route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_handshakes_are_bounded_and_timed_out() {
  let ca = Ca::new("ca");
  let mut limits = QuicLimits::default();
  limits.max_pending_handshakes = 1;
  limits.handshake_timeout = Duration::from_millis(500);
  let endpoint = endpoint_with(&ca, limits);
  let h3 = endpoint.register(Route::h3()).expect("h3");
  let addr = endpoint.local_addr();

  // A client stuck checking the server certificate, on its own thread.
  let (release, stalled) = std::sync::mpsc::channel();
  let provider = id_effect_platform::tls::provider();
  let stall = Arc::new(Stall {
    release: Mutex::new(stalled),
    provider: Arc::clone(&provider),
  });
  let stuck = std::thread::spawn(move || {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()
      .expect("runtime");
    runtime.block_on(async move {
      let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("tls13")
        .dangerous()
        .with_custom_certificate_verifier(stall)
        .with_no_client_auth();
      let _ = dial(tls, &[ALPN_H3], addr, "localhost").await;
    });
  });

  // While it holds the only slot, the next client is refused.
  until(&endpoint, |s| s.retries_sent >= 1).await;
  tokio::time::sleep(Duration::from_millis(100)).await;
  assert!(
    dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
      .await
      .is_err()
  );
  assert!(endpoint.stats().refused_busy >= 1);

  // The stuck handshake times out and its route hears about it.
  let refused = next(&h3).await.expect_err("timed out");
  assert!(
    matches!(&refused, Refusal::Handshake { reason, .. } if reason.contains("timed out")),
    "{refused:?}"
  );
  let _ = release.send(());
  stuck.join().expect("client thread");

  // The slot is free again.
  dial(tls(&ca, None), &[ALPN_H3], addr, "localhost")
    .await
    .expect("after the timeout");
}
