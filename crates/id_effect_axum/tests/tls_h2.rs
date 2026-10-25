//! TLS and HTTP/2 on TCP (feature `tls`, ADR 0014 D10) over real sockets: ALPN `h2` and
//! `http/1.1` from one listener, a reloaded certificate on new connections only, the HTTP/2
//! concurrent-stream cap and request limit, the TLS handshake timeout, and the host refusing to
//! start on a certificate it cannot load.
#![cfg(feature = "tls")]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::ConnectInfo;
use axum::http::{Request, Version, header};
use axum::routing::get;
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::{TokioExecutor, TokioIo};
use id_effect_axum::server::tls::ServerTls;
use id_effect_axum::server::{HostTls, serve_router_with_limits, serve_tls_listener};
use id_effect_axum::{ConnectionLimits, HostConfig, HostError, ReadinessState, ServerHandle};
use id_effect_platform::tls::{Identity, NodeCertificate, provider, sha256};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

const WAIT: Duration = Duration::from_secs(5);

/// A test CA issuing `localhost` certificates.
struct Ca {
  cert: rcgen::Certificate,
  issuer: rcgen::Issuer<'static, KeyPair>,
}

/// A leaf certificate: PEM chain, PEM key and the leaf's SHA-256.
struct Leaf {
  chain_pem: String,
  key_pem: String,
  sha256: [u8; 32],
}

impl Ca {
  fn new() -> Self {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(DnType::CommonName, "id_effect test CA");
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::new(params, key);
    Self { cert, issuer }
  }

  fn issue(&self) -> Leaf {
    let params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &self.issuer).unwrap();
    Leaf {
      chain_pem: cert.pem(),
      key_pem: key.serialize_pem(),
      sha256: sha256(cert.der()),
    }
  }

  fn roots(&self) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add(self.cert.der().clone()).unwrap();
    roots
  }
}

impl Leaf {
  fn identity(&self) -> Identity {
    let cert =
      NodeCertificate::from_pem(self.chain_pem.as_bytes(), self.key_pem.as_bytes()).unwrap();
    Identity::from_certificate(&cert).unwrap()
  }

  fn write_to(&self, dir: &Path) -> (PathBuf, PathBuf) {
    let (chain, key) = (dir.join("chain.pem"), dir.join("key.pem"));
    std::fs::write(&chain, &self.chain_pem).unwrap();
    std::fs::write(&key, &self.key_pem).unwrap();
    (chain, key)
  }
}

fn local_config() -> HostConfig {
  HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: 0,
    shutdown_timeout: Duration::from_secs(5),
    ..HostConfig::default()
  }
}

async fn start(tls: HostTls, app: Router, limits: ConnectionLimits) -> ServerHandle {
  ServerHandle::start_with_limits(
    &local_config().with_tls(tls),
    app,
    ReadinessState::new(true),
    limits,
  )
  .await
  .unwrap()
}

/// `/ip` answers the peer address, `/slow` sleeps 150 ms while counting concurrent requests.
fn app(active: Arc<AtomicUsize>, peak: Arc<AtomicUsize>) -> Router {
  Router::new()
    .route(
      "/ip",
      get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
    )
    .route(
      "/slow",
      get(move || {
        let (active, peak) = (Arc::clone(&active), Arc::clone(&peak));
        async move {
          let now = active.fetch_add(1, Ordering::SeqCst) + 1;
          peak.fetch_max(now, Ordering::SeqCst);
          tokio::time::sleep(Duration::from_millis(150)).await;
          active.fetch_sub(1, Ordering::SeqCst);
          "done"
        }
      }),
    )
}

fn plain_app() -> Router {
  app(Arc::default(), Arc::default())
}

fn client_config(roots: RootCertStore, alpn: &[&[u8]]) -> Arc<ClientConfig> {
  let mut config = ClientConfig::builder_with_provider(provider())
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
  config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
  Arc::new(config)
}

async fn tls_connect(addr: SocketAddr, config: Arc<ClientConfig>) -> TlsStream<TcpStream> {
  let tcp = TcpStream::connect(addr).await.unwrap();
  let name = ServerName::try_from("localhost").unwrap();
  tokio::time::timeout(
    WAIT,
    tokio_rustls::TlsConnector::from(config).connect(name, tcp),
  )
  .await
  .expect("a handshake in time")
  .unwrap()
}

fn peer_sha256(stream: &TlsStream<TcpStream>) -> [u8; 32] {
  sha256(&stream.get_ref().1.peer_certificates().unwrap()[0])
}

fn get_req(uri: &str) -> Request<Empty<Bytes>> {
  Request::get(uri)
    .header(header::HOST, "localhost")
    .body(Empty::new())
    .unwrap()
}

async fn body_text(res: hyper::Response<hyper::body::Incoming>) -> String {
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  String::from_utf8(bytes.to_vec()).unwrap()
}

type H1 = hyper::client::conn::http1::SendRequest<Empty<Bytes>>;
type H2 = hyper::client::conn::http2::SendRequest<Empty<Bytes>>;

async fn h1(stream: TlsStream<TcpStream>) -> (H1, tokio::task::JoinHandle<()>) {
  let (send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
    .await
    .unwrap();
  (
    send,
    tokio::spawn(async move {
      let _ = conn.await;
    }),
  )
}

async fn h2(stream: TlsStream<TcpStream>) -> (H2, tokio::task::JoinHandle<()>) {
  let (send, conn) =
    hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
      .await
      .unwrap();
  (
    send,
    tokio::spawn(async move {
      let _ = conn.await;
    }),
  )
}

/// Reads until the server closes the connection; `true` when it did within `within`.
async fn closed_within<S: tokio::io::AsyncRead + Unpin>(stream: &mut S, within: Duration) -> bool {
  let mut buf = [0_u8; 512];
  let deadline = Instant::now() + within;
  loop {
    let left = deadline.saturating_duration_since(Instant::now());
    match tokio::time::timeout(left, stream.read(&mut buf)).await {
      Err(_) => return false,
      Ok(Ok(0) | Err(_)) => return true,
      Ok(Ok(_)) => {}
    }
  }
}

#[tokio::test]
async fn tls_h2_round_trip() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let tls = ServerTls::from_identity(&leaf.identity()).unwrap();
  let server = start(
    HostTls::provided(tls),
    plain_app(),
    ConnectionLimits::default(),
  )
  .await;
  let stream = tls_connect(
    server.local_addr(),
    client_config(ca.roots(), &[b"h2", b"http/1.1"]),
  )
  .await;
  assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
  assert_eq!(peer_sha256(&stream), leaf.sha256);
  let (mut send, _conn) = h2(stream).await;
  let res = send
    .send_request(get_req("https://localhost/ip"))
    .await
    .unwrap();
  assert_eq!(res.version(), Version::HTTP_2);
  assert_eq!(res.status(), 200);
  assert_eq!(body_text(res).await, "127.0.0.1");
  server.shutdown().await;
}

#[tokio::test]
async fn tls_http1_round_trip() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let server = start(
    HostTls::provided(ServerTls::from_identity(&leaf.identity()).unwrap()),
    plain_app(),
    ConnectionLimits::default(),
  )
  .await;
  let addr = server.local_addr();
  // ALPN `http/1.1`, and no ALPN at all: both HTTP/1.1.
  for alpn in [&[&b"http/1.1"[..]][..], &[]] {
    let stream = tls_connect(addr, client_config(ca.roots(), alpn)).await;
    let (mut send, _conn) = h1(stream).await;
    let res = send.send_request(get_req("/ip")).await.unwrap();
    assert_eq!(res.version(), Version::HTTP_11);
    assert_eq!(body_text(res).await, "127.0.0.1");
  }
  // Plain HTTP on the TLS port is not served.
  let mut plain = TcpStream::connect(addr).await.unwrap();
  plain
    .write_all(b"GET /ip HTTP/1.1\r\nHost: localhost\r\n\r\n")
    .await
    .unwrap();
  let mut out = Vec::new();
  let _ = tokio::time::timeout(WAIT, plain.read_to_end(&mut out)).await;
  assert!(!out.starts_with(b"HTTP/"), "{out:?}");
  server.shutdown().await;
}

#[tokio::test]
async fn tls12_allowed_unless_tls13_only() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let tls12_client = || {
    let mut config = ClientConfig::builder_with_provider(provider())
      .with_protocol_versions(&[&tokio_rustls::rustls::version::TLS12])
      .unwrap()
      .with_root_certificates(ca.roots())
      .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Arc::new(config)
  };
  let both = start(
    HostTls::provided(ServerTls::from_identity(&leaf.identity()).unwrap()),
    plain_app(),
    ConnectionLimits::default(),
  )
  .await;
  let stream = tls_connect(both.local_addr(), tls12_client()).await;
  assert_eq!(
    stream.get_ref().1.protocol_version(),
    Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_2)
  );
  let (mut send, _conn) = h2(stream).await;
  let res = send
    .send_request(get_req("https://localhost/ip"))
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  both.shutdown().await;

  let strict = ServerTls::builder(Arc::new(leaf.identity()))
    .tls13_only()
    .build()
    .unwrap();
  let only13 = start(
    HostTls::provided(strict),
    plain_app(),
    ConnectionLimits::default(),
  )
  .await;
  let tcp = TcpStream::connect(only13.local_addr()).await.unwrap();
  let refused = tokio_rustls::TlsConnector::from(tls12_client())
    .connect(ServerName::try_from("localhost").unwrap(), tcp)
    .await;
  assert!(refused.is_err(), "a TLS 1.2 client got through");
  only13.shutdown().await;
}

#[tokio::test]
async fn tls_reloaded_cert_used_for_new_connections() {
  let ca = Ca::new();
  let (first, second) = (ca.issue(), ca.issue());
  let dir = std::env::temp_dir().join(format!(
    "id_effect_axum_tls_reload_{}_{}",
    std::process::id(),
    first.sha256[0]
  ));
  std::fs::create_dir_all(&dir).unwrap();
  let (chain, key) = first.write_to(&dir);
  let files = HostTls::Files {
    cert_file: chain,
    key_file: key,
    reload_every: Some(Duration::from_millis(50)),
  };
  let server = start(files, plain_app(), ConnectionLimits::default()).await;
  let addr = server.local_addr();
  let config = client_config(ca.roots(), &[b"http/1.1"]);

  let old = tls_connect(addr, Arc::clone(&config)).await;
  assert_eq!(peer_sha256(&old), first.sha256);
  let (mut old_send, _old_conn) = h1(old).await;
  assert_eq!(
    old_send
      .send_request(get_req("/ip"))
      .await
      .unwrap()
      .status(),
    200
  );

  second.write_to(&dir);
  let deadline = Instant::now() + WAIT;
  loop {
    let fresh = tls_connect(addr, Arc::clone(&config)).await;
    if peer_sha256(&fresh) == second.sha256 {
      let (mut send, _conn) = h1(fresh).await;
      assert_eq!(
        send.send_request(get_req("/ip")).await.unwrap().status(),
        200
      );
      break;
    }
    assert!(
      Instant::now() < deadline,
      "the rotated certificate was never served"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  // The connection opened before the rotation keeps working on its certificate.
  let res = old_send.send_request(get_req("/ip")).await.unwrap();
  assert_eq!(body_text(res).await, "127.0.0.1");
  server.shutdown().await;
  let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn h2_concurrent_streams_capped() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let (active, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
  let server = start(
    HostTls::provided(ServerTls::from_identity(&leaf.identity()).unwrap()),
    app(Arc::clone(&active), Arc::clone(&peak)),
    ConnectionLimits::new().max_concurrent_streams(2),
  )
  .await;
  let stream = tls_connect(server.local_addr(), client_config(ca.roots(), &[b"h2"])).await;
  let (mut send, _conn) = h2(stream).await;
  // One round trip first, so the client has the server's SETTINGS before it multiplexes.
  let warm_up = send
    .send_request(get_req("https://localhost/ip"))
    .await
    .unwrap();
  assert_eq!(warm_up.status(), 200);
  let requests = (0..6).map(|_| {
    let mut send = send.clone();
    tokio::spawn(async move {
      let res = send
        .send_request(get_req("https://localhost/slow"))
        .await
        .unwrap();
      (res.status(), body_text(res).await)
    })
  });
  for request in requests.collect::<Vec<_>>() {
    let (status, body) = tokio::time::timeout(WAIT, request)
      .await
      .expect("every request is served in time")
      .unwrap();
    assert_eq!((status.as_u16(), body.as_str()), (200, "done"));
  }
  assert_eq!(peak.load(Ordering::SeqCst), 2, "at most 2 streams at once");
  server.shutdown().await;
}

#[tokio::test]
async fn h2_connection_goes_away_after_max_requests() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let server = start(
    HostTls::provided(ServerTls::from_identity(&leaf.identity()).unwrap()),
    plain_app(),
    ConnectionLimits::new().max_requests_per_connection(2),
  )
  .await;
  let stream = tls_connect(server.local_addr(), client_config(ca.roots(), &[b"h2"])).await;
  let (mut send, conn) = h2(stream).await;
  for _ in 0..2 {
    let res = send
      .send_request(get_req("https://localhost/ip"))
      .await
      .unwrap();
    assert_eq!(res.status(), 200);
  }
  tokio::time::timeout(WAIT, conn)
    .await
    .expect("the server closes the connection after its request limit")
    .unwrap();
  server.shutdown().await;
}

#[tokio::test]
async fn tls_handshake_timeout_closes_slow_client() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let server = start(
    HostTls::provided(ServerTls::from_identity(&leaf.identity()).unwrap()),
    plain_app(),
    ConnectionLimits::new()
      .tls_handshake_timeout(Duration::from_millis(200))
      .header_read_timeout(Duration::from_secs(30)),
  )
  .await;
  let started = Instant::now();
  let mut silent = TcpStream::connect(server.local_addr()).await.unwrap();
  assert!(
    closed_within(&mut silent, WAIT).await,
    "the server keeps a silent client"
  );
  assert!(
    started.elapsed() < Duration::from_secs(3),
    "{:?}",
    started.elapsed()
  );
  // A complete handshake is still served.
  let stream = tls_connect(server.local_addr(), client_config(ca.roots(), &[b"h2"])).await;
  let (mut send, _conn) = h2(stream).await;
  let res = send
    .send_request(get_req("https://localhost/ip"))
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  server.shutdown().await;
}

#[tokio::test]
async fn shutdown_drops_a_client_stuck_in_the_handshake() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let server = start(
    HostTls::provided(ServerTls::from_identity(&leaf.identity()).unwrap()),
    plain_app(),
    ConnectionLimits::new().dangerous_no_tls_handshake_timeout(),
  )
  .await;
  let mut silent = TcpStream::connect(server.local_addr()).await.unwrap();
  tokio::time::sleep(Duration::from_millis(100)).await;
  let started = Instant::now();
  server.shutdown().await;
  assert!(
    started.elapsed() < Duration::from_secs(3),
    "{:?}",
    started.elapsed()
  );
  assert!(closed_within(&mut silent, WAIT).await);
}

#[tokio::test]
async fn extra_alpn_handshakes_then_closes_without_http() {
  let ca = Ca::new();
  let leaf = ca.issue();
  let tls = ServerTls::builder(Arc::new(leaf.identity()))
    .alpn("acme-tls/1")
    .build()
    .unwrap();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
  let serving = tokio::spawn(serve_tls_listener(
    listener,
    plain_app(),
    ConnectionLimits::default(),
    tls,
    async move {
      let _ = stopped.await;
    },
  ));
  let mut acme = tls_connect(addr, client_config(ca.roots(), &[b"acme-tls/1"])).await;
  assert_eq!(acme.get_ref().1.alpn_protocol(), Some(&b"acme-tls/1"[..]));
  assert!(closed_within(&mut acme, WAIT).await);
  // The same listener serves HTTP/2.
  let stream = tls_connect(addr, client_config(ca.roots(), &[b"h2"])).await;
  let (mut send, _conn) = h2(stream).await;
  let res = send
    .send_request(get_req("https://localhost/ip"))
    .await
    .unwrap();
  assert_eq!(res.status(), 200);
  drop(send);
  stop.send(()).unwrap();
  tokio::time::timeout(WAIT, serving)
    .await
    .expect("the listener stops")
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn host_refuses_to_start_on_an_unloadable_certificate() {
  let missing = HostTls::files("/nonexistent/chain.pem", "/nonexistent/key.pem");
  let config = local_config().with_tls(missing);
  let err = ServerHandle::start(&config, plain_app()).await.unwrap_err();
  assert!(
    matches!(&err, HostError::Serve(m) if m.starts_with("tls:")),
    "{err}"
  );
  let host = id_effect_axum::Host {
    config,
    env: id_effect_axum::bootstrap_env(id_effect_config::MapConfigProvider::from_pairs([(
      "PORT", "1",
    )])),
    drain: Arc::default(),
    readiness: ReadinessState::new(true),
  };
  let err = serve_router_with_limits(host, plain_app(), ConnectionLimits::default())
    .await
    .unwrap_err();
  assert!(matches!(err, HostError::Serve(_)), "{err}");
}
