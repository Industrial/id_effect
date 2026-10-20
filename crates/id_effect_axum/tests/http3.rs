//! HTTP/3 (feature `http3`, r12-e12-3-h3-server) over real sockets: the same router on TCP and
//! UDP, typed routes / middleware / guards unchanged, `Alt-Svc` on TLS TCP responses, one flag
//! to turn it off, both listeners drained on shutdown, 0-RTT off, the client IP from the QUIC
//! peer, and the connection limits on HTTP/3.
#![cfg(feature = "http3")]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request as AxumRequest};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Router};
use bytes::{Buf, Bytes, BytesMut};
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper_util::rt::TokioIo;
use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, fail, succeed};
use id_effect_axum::route::{GuardFuture, Input, Route, RouteGuard, Router as TypedRouter};
use id_effect_axum::server::h3::{Http3Options, MAX_FIELD_SECTION_SIZE, quic_limits, serve_h3};
use id_effect_axum::server::tls::ServerTls;
use id_effect_axum::server::{HostTls, serve_router_with_limits};
use id_effect_axum::{
  ClientIp, ConnectionLimits, HostConfig, HostError, ReadinessState, ServerHandle, TrustedProxies,
};
use id_effect_platform::quic::{
  ALPN_H3, QuicEndpoint, QuicEndpointConfig, QuicLimits, Route as QuicRoute, client_config,
};
use id_effect_platform::tls::{Identity, NodeCertificate, provider};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};

/// Generous: the suite runs beside trybuild compilations that load every core.
const WAIT: Duration = Duration::from_secs(20);

// --- certificates --------------------------------------------------------------------------

struct Ca {
  cert: rcgen::Certificate,
  key: KeyPair,
}

struct Leaf {
  chain_pem: String,
  key_pem: String,
}

impl Ca {
  fn new() -> Self {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(DnType::CommonName, "id_effect h3 test CA");
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    Self { cert, key }
  }

  fn issue(&self) -> Leaf {
    let params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &self.cert, &self.key).unwrap();
    Leaf {
      chain_pem: cert.pem(),
      key_pem: key.serialize_pem(),
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

  /// The PEM files in a fresh directory, as the host's TLS settings.
  fn files(&self) -> HostTls {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "id-effect-h3-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (chain, key): (PathBuf, PathBuf) = (dir.join("chain.pem"), dir.join("key.pem"));
    std::fs::write(&chain, &self.chain_pem).unwrap();
    std::fs::write(&key, &self.key_pem).unwrap();
    HostTls::files(chain, key)
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

async fn start(ca: &Ca, app: Router, limits: ConnectionLimits) -> ServerHandle {
  ServerHandle::start_with_limits(
    &local_config().with_tls(ca.issue().files()),
    app,
    ReadinessState::new(true),
    limits,
  )
  .await
  .unwrap()
}

// --- an HTTP/3 client ------------------------------------------------------------------------

type SendRequest = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

struct H3 {
  conn: quinn::Connection,
  send: SendRequest,
  _endpoint: quinn::Endpoint,
}

/// Client limits with a 3 s idle timeout, so a dead server is noticed quickly (and a loaded
/// machine still completes handshakes).
fn quick_limits() -> QuicLimits {
  let mut limits = QuicLimits::default();
  limits.idle_timeout = Duration::from_secs(3);
  limits
}

fn quic_client(roots: RootCertStore, early_data: bool) -> quinn::ClientConfig {
  quic_client_with(roots, early_data, &quick_limits())
}

fn quic_client_with(
  roots: RootCertStore,
  early_data: bool,
  limits: &QuicLimits,
) -> quinn::ClientConfig {
  let mut tls = ClientConfig::builder_with_provider(provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
  tls.enable_early_data = early_data;
  if early_data {
    tls.alpn_protocols = vec![ALPN_H3.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(
      id_effect_platform::quic::transport_config(limits).unwrap(),
    ));
    return config;
  }
  client_config(tls, &[ALPN_H3], limits).unwrap()
}

async fn quic_connect(
  addr: SocketAddr,
  config: quinn::ClientConfig,
) -> Result<(quinn::Endpoint, quinn::Connection), String> {
  let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
  let connecting = endpoint
    .connect_with(config, addr, "localhost")
    .map_err(|e| e.to_string())?;
  let conn = tokio::time::timeout(WAIT, connecting)
    .await
    .map_err(|_| "handshake timed out".to_string())?
    .map_err(|e| e.to_string())?;
  Ok((endpoint, conn))
}

async fn h3_connect(addr: SocketAddr, roots: RootCertStore) -> Result<H3, String> {
  h3_connect_with(addr, quic_client(roots, false)).await
}

async fn h3_connect_with(addr: SocketAddr, config: quinn::ClientConfig) -> Result<H3, String> {
  let (endpoint, conn) = quic_connect(addr, config).await?;
  let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(conn.clone()))
    .await
    .map_err(|e| e.to_string())?;
  tokio::spawn(async move {
    let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
  });
  Ok(H3 {
    conn,
    send,
    _endpoint: endpoint,
  })
}

/// One HTTP/3 response: status, headers, body, trailers.
struct H3Response {
  status: StatusCode,
  headers: HeaderMap,
  body: String,
  trailers: Option<HeaderMap>,
}

impl H3 {
  async fn request(
    &mut self,
    request: Request<()>,
    body: Option<&str>,
    trailers: Option<HeaderMap>,
  ) -> Result<H3Response, String> {
    let err = |e: h3::error::StreamError| e.to_string();
    let mut stream = self.send.send_request(request).await.map_err(err)?;
    if let Some(body) = body {
      stream
        .send_data(Bytes::copy_from_slice(body.as_bytes()))
        .await
        .map_err(err)?;
    }
    if let Some(trailers) = trailers {
      stream.send_trailers(trailers).await.map_err(err)?;
    }
    stream.finish().await.map_err(err)?;
    let response = stream.recv_response().await.map_err(err)?;
    let mut bytes = BytesMut::new();
    while let Some(mut chunk) = stream.recv_data().await.map_err(err)? {
      bytes.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    let trailers = stream.recv_trailers().await.map_err(err)?;
    Ok(H3Response {
      status: response.status(),
      headers: response.headers().clone(),
      body: String::from_utf8(bytes.to_vec()).unwrap(),
      trailers,
    })
  }

  async fn get(&mut self, path: &str) -> Result<H3Response, String> {
    self.request(req(Method::GET, path), None, None).await
  }
}

fn req(method: Method, path: &str) -> Request<()> {
  Request::builder()
    .method(method)
    .uri(format!("https://localhost{path}"))
    .body(())
    .unwrap()
}

// --- a TLS TCP client ------------------------------------------------------------------------

async fn tcp_get(
  addr: SocketAddr,
  roots: RootCertStore,
  path: &str,
) -> (StatusCode, HeaderMap, String) {
  let mut config = ClientConfig::builder_with_provider(provider())
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
  config.alpn_protocols = vec![b"http/1.1".to_vec()];
  let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
  let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
    .connect(ServerName::try_from("localhost").unwrap(), tcp)
    .await
    .unwrap();
  let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
    .await
    .unwrap();
  tokio::spawn(async move {
    let _ = conn.await;
  });
  let request = Request::get(path)
    .header(header::HOST, "localhost")
    .body(Empty::<Bytes>::new())
    .unwrap();
  let response = send.send_request(request).await.unwrap();
  let (parts, body) = response.into_parts();
  let body = body.collect().await.unwrap().to_bytes();
  (
    parts.status,
    parts.headers,
    String::from_utf8(body.to_vec()).unwrap(),
  )
}

async fn plain_get(addr: SocketAddr, path: &str) -> HeaderMap {
  let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
  let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
    .await
    .unwrap();
  tokio::spawn(async move {
    let _ = conn.await;
  });
  let request = Request::get(path)
    .header(header::HOST, "localhost")
    .body(Empty::<Bytes>::new())
    .unwrap();
  send.send_request(request).await.unwrap().headers().clone()
}

// --- apps --------------------------------------------------------------------------------------

struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    schema::i64()
  }
}

struct NewAccount;

impl HasSchema for NewAccount {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_("name", schema::string(), "age", schema::i64())
  }
}

fn by_id(_: &mut (), input: Input<i64>) -> Effect<String, StatusCode, ()> {
  if input.path == 404 {
    fail(StatusCode::NOT_FOUND)
  } else {
    succeed(format!("id={}", input.path))
  }
}

fn create(
  _: &mut (),
  input: Input<(), (), (), (String, i64)>,
) -> Effect<(StatusCode, String), Infallible, ()> {
  let (name, age) = input.body;
  succeed((StatusCode::CREATED, format!("{name}:{age}")))
}

fn ok(_: &mut (), _: Input) -> Effect<&'static str, Infallible, ()> {
  succeed("admin ok")
}

/// Admits requests whose `x-role` header is `admin`; answers 403 otherwise.
struct Admin;

impl RouteGuard for Admin {
  fn policy(&self) -> &str {
    "admin"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let allowed = parts.headers.get("x-role").is_some_and(|v| v == "admin");
    Box::pin(async move {
      if allowed {
        Ok(())
      } else {
        Err((StatusCode::FORBIDDEN, "not admin").into_response())
      }
    })
  }
}

fn typed_app() -> Router {
  TypedRouter::new()
    .route(
      Route::get("/ids/{id}")
        .public()
        .path::<AccountId>()
        .handler(by_id),
    )
    .route(
      Route::post("/accounts")
        .public()
        .body::<NewAccount>()
        .handler(create),
    )
    .route(Route::get("/admin").guard(Admin).handler(ok))
    .merge_axum(Router::new().route("/undeclared", get(|| async { "internals" })))
    .into_axum()
}

/// `/hello`, `/slow` (300 ms), `/ip` (the peer), `/client-ip` (behind trusted proxies),
/// `/echo` (body and trailer `x-sum`), `/trailers`, `/broken` (a body that fails).
fn plain_app() -> Router {
  Router::new()
    .route("/hello", get(|| async { "hello" }))
    .route(
      "/slow",
      get(|| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "done"
      }),
    )
    .route(
      "/ip",
      get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.to_string() }),
    )
    .route(
      "/client-ip",
      get(|ClientIp(ip): ClientIp| async move { ip.to_string() }),
    )
    .route(
      "/echo",
      post(|request: AxumRequest| async move {
        match request.into_body().collect().await {
          Ok(collected) => {
            let sum = collected
              .trailers()
              .and_then(|t| t.get("x-sum"))
              .map(|v| v.to_str().unwrap().to_owned())
              .unwrap_or_default();
            let body = String::from_utf8(collected.to_bytes().to_vec()).unwrap();
            format!("{body}|{sum}")
          }
          Err(_) => "body error".to_owned(),
        }
      }),
    )
    .route(
      "/trailers",
      get(|| async {
        let mut trailers = HeaderMap::new();
        trailers.insert("x-done", HeaderValue::from_static("yes"));
        let frames = futures_util::stream::iter([
          Ok::<_, Infallible>(http_body::Frame::data(Bytes::from_static(b"part"))),
          Ok(http_body::Frame::data(Bytes::new())),
          Ok(http_body::Frame::trailers(trailers)),
        ]);
        Response::new(Body::new(StreamBody::new(frames)))
      }),
    )
    .route(
      "/broken",
      get(|| async {
        let chunks = futures_util::stream::iter([
          Ok::<_, std::io::Error>(Bytes::from_static(b"start")),
          Err(std::io::Error::other("disk gone")),
        ]);
        Body::from_stream(chunks)
      }),
    )
    // Answer without reading the body; `/keep` also holds the request for a while.
    .route("/early", post(|| async { "early" }))
    .route(
      "/keep",
      post(|request: AxumRequest| async move {
        tokio::spawn(async move {
          tokio::time::sleep(Duration::from_millis(200)).await;
          drop(request);
        });
        "kept"
      }),
    )
    .route(
      "/hop",
      get(|| async { ([(header::CONNECTION, "keep-alive")], "hop") }),
    )
}

// --- acceptance tests --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_serves_typed_routes_unchanged() {
  let ca = Ca::new();
  let handle = start(&ca, typed_app(), ConnectionLimits::default()).await;
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();

  let found = h3.get("/ids/7").await.unwrap();
  assert_eq!(
    (found.status, found.body.as_str()),
    (StatusCode::OK, "id=7")
  );
  assert_eq!(
    h3.get("/ids/404").await.unwrap().status,
    StatusCode::NOT_FOUND
  );
  // Path decoding failures are the same RFC 9457 problem as on TCP.
  let bad = h3.get("/ids/seven").await.unwrap();
  assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(
    bad.headers[header::CONTENT_TYPE],
    id_effect_axum::PROBLEM_JSON
  );

  let mut create = req(Method::POST, "/accounts");
  create.headers_mut().insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("application/json"),
  );
  let created = h3
    .request(create, Some(r#"{"name":"ada","age":36}"#), None)
    .await
    .unwrap();
  assert_eq!(
    (created.status, created.body.as_str()),
    (StatusCode::CREATED, "ada:36")
  );
  // A plain route without an access decision is denied over HTTP/3 too (H8).
  let undeclared = h3.get("/undeclared").await.unwrap();
  assert_eq!(undeclared.status, StatusCode::NOT_FOUND);
  assert!(!undeclared.body.contains("internals"));
  // HEAD: the head of GET, no body.
  let head = h3
    .request(req(Method::HEAD, "/ids/7"), None, None)
    .await
    .unwrap();
  assert_eq!((head.status, head.body.as_str()), (StatusCode::OK, ""));
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_middleware_and_guards_apply() {
  let ca = Ca::new();
  let app = typed_app().layer(axum::middleware::map_response(
    |mut response: Response<Body>| async move {
      response
        .headers_mut()
        .insert("x-middleware", HeaderValue::from_static("ran"));
      response
    },
  ));
  let handle = start(&ca, app, ConnectionLimits::default()).await;
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();

  let denied = h3.get("/admin").await.unwrap();
  assert_eq!(denied.status, StatusCode::FORBIDDEN);
  assert_eq!(denied.headers["x-middleware"], "ran");
  let mut admin = req(Method::GET, "/admin");
  admin
    .headers_mut()
    .insert("x-role", HeaderValue::from_static("admin"));
  let allowed = h3.request(admin, None, None).await.unwrap();
  assert_eq!(
    (allowed.status, allowed.body.as_str()),
    (StatusCode::OK, "admin ok")
  );
  assert_eq!(allowed.headers["x-middleware"], "ran");
  // HTTP/3 responses do not advertise HTTP/3.
  assert!(allowed.headers.get(header::ALT_SVC).is_none());
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alt_svc_header_on_tcp_responses() {
  let ca = Ca::new();
  let handle = start(&ca, plain_app(), ConnectionLimits::default()).await;
  let addr = handle.local_addr();
  let (status, headers, body) = tcp_get(addr, ca.roots(), "/hello").await;
  assert_eq!((status, body.as_str()), (StatusCode::OK, "hello"));
  assert_eq!(
    headers[header::ALT_SVC],
    format!("h3=\":{}\"; ma=86400", addr.port()).as_str()
  );
  // The advertised port is the TCP port: UDP listens on the same number.
  let mut h3 = h3_connect(addr, ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http3_disabled_by_one_flag() {
  let ca = Ca::new();
  let config = HostConfig {
    http3: false,
    ..local_config()
  }
  .with_tls(ca.issue().files());
  let handle = ServerHandle::start_with_limits(
    &config,
    plain_app(),
    ReadinessState::new(true),
    ConnectionLimits::default(),
  )
  .await
  .unwrap();
  let (status, headers, _) = tcp_get(handle.local_addr(), ca.roots(), "/hello").await;
  assert_eq!(status, StatusCode::OK);
  assert!(headers.get(header::ALT_SVC).is_none());
  assert!(h3_connect(handle.local_addr(), ca.roots()).await.is_err());
  handle.shutdown().await;

  // HTTP/3 on, but plain HTTP: no UDP, no Alt-Svc (the posture warning says why).
  let handle = ServerHandle::start(&local_config(), plain_app())
    .await
    .unwrap();
  assert!(
    plain_get(handle.local_addr(), "/hello")
      .await
      .get(header::ALT_SVC)
      .is_none()
  );
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_tcp_and_udp() {
  let ca = Ca::new();
  let handle = Arc::new(start(&ca, plain_app(), ConnectionLimits::default()).await);
  let addr = handle.local_addr();
  let mut h3 = h3_connect(addr, ca.roots()).await.unwrap();
  let roots = ca.roots();
  let tcp = tokio::spawn(async move { tcp_get(addr, roots, "/slow").await });
  let udp = tokio::spawn(async move {
    let response = h3.get("/slow").await;
    (response, h3)
  });
  tokio::time::sleep(Duration::from_millis(100)).await;
  let started = std::time::Instant::now();
  handle.shutdown().await;
  // Both in-flight requests finished before the host stopped.
  let (status, _, body) = tcp.await.unwrap();
  assert_eq!((status, body.as_str()), (StatusCode::OK, "done"));
  let (response, mut h3) = udp.await.unwrap();
  let response = response.unwrap();
  assert_eq!(
    (response.status, response.body.as_str()),
    (StatusCode::OK, "done")
  );
  assert!(started.elapsed() < Duration::from_secs(5));
  // After the drain: GOAWAY, then no new request on the old connection, no new connections.
  assert!(h3.get("/hello").await.is_err());
  assert!(tokio::net::TcpStream::connect(addr).await.is_err());
  assert!(h3_connect(addr, ca.roots()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_rtt_off_by_default() {
  assert!(!Http3Options::default().zero_rtt_for_queries());
  let ca = Ca::new();
  let handle = start(&ca, plain_app(), ConnectionLimits::default()).await;
  // One client config: its session cache keeps the server's ticket for the second dial.
  let config = quic_client(ca.roots(), true);
  let (_endpoint, first) = quic_connect(handle.local_addr(), config.clone())
    .await
    .unwrap();
  // Give the server time to send its session ticket, then resume.
  tokio::time::sleep(Duration::from_millis(100)).await;
  first.close(0u32.into(), b"bye");
  let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
  let connecting = endpoint
    .connect_with(config, handle.local_addr(), "localhost")
    .unwrap();
  // The ticket allows no early data: the client cannot send 0-RTT, it waits for the handshake.
  let Err(connecting) = connecting.into_0rtt() else {
    panic!("the server offered 0-RTT");
  };
  tokio::time::timeout(WAIT, connecting)
    .await
    .unwrap()
    .unwrap();
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_client_ip_from_quic_peer() {
  let ca = Ca::new();
  // No trusted proxies: X-Forwarded-For is ignored, the QUIC peer is the client.
  let handle = start(&ca, plain_app(), ConnectionLimits::default()).await;
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  let local = h3.conn.local_ip().unwrap_or("127.0.0.1".parse().unwrap());
  let peer = h3.get("/ip").await.unwrap().body;
  let peer: SocketAddr = peer.parse().unwrap();
  assert_eq!(peer.ip(), local);
  let mut forged = req(Method::GET, "/client-ip");
  forged
    .headers_mut()
    .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
  assert_eq!(
    h3.request(forged, None, None).await.unwrap().body,
    "127.0.0.1"
  );
  handle.shutdown().await;

  // The QUIC peer is a trusted proxy: its X-Forwarded-For entry names the client.
  let trusted = TrustedProxies::new(["127.0.0.1/32"]).unwrap();
  let handle = start(
    &ca,
    plain_app().layer(Extension(trusted)),
    ConnectionLimits::default(),
  )
  .await;
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  let mut forwarded = req(Method::GET, "/client-ip");
  forwarded
    .headers_mut()
    .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
  assert_eq!(
    h3.request(forwarded, None, None).await.unwrap().body,
    "203.0.113.9"
  );
  handle.shutdown().await;
}

// --- limits and protocol details -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_request_bodies_trailers_and_failing_bodies() {
  let ca = Ca::new();
  let handle = start(&ca, plain_app(), ConnectionLimits::default()).await;
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  let mut trailers = HeaderMap::new();
  trailers.insert("x-sum", HeaderValue::from_static("42"));
  let echoed = h3
    .request(req(Method::POST, "/echo"), Some("payload"), Some(trailers))
    .await
    .unwrap();
  assert_eq!(echoed.body, "payload|42");
  let plain = h3
    .request(req(Method::POST, "/echo"), Some("x"), None)
    .await
    .unwrap();
  assert_eq!(plain.body, "x|");

  let with_trailers = h3.get("/trailers").await.unwrap();
  assert_eq!(with_trailers.body, "part");
  assert_eq!(with_trailers.trailers.unwrap()["x-done"], "yes");
  // A body that fails mid-way resets the stream: the client sees an error, not a short body.
  assert!(h3.get("/broken").await.is_err());
  // Connection-specific headers never reach HTTP/3.
  let hop = h3.get("/hop").await.unwrap();
  assert_eq!(hop.body, "hop");
  assert!(hop.headers.get(header::CONNECTION).is_none());

  // A request body the client resets: the handler sees a body error, the response still goes.
  let mut stream = h3
    .send
    .send_request(req(Method::POST, "/echo"))
    .await
    .unwrap();
  stream.send_data(Bytes::from_static(b"half")).await.unwrap();
  // Let the handler start reading, then reset the request body under it.
  tokio::time::sleep(Duration::from_millis(150)).await;
  stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
  let response = tokio::time::timeout(WAIT, stream.recv_response())
    .await
    .unwrap();
  if let Ok(response) = response {
    assert_eq!(response.status(), StatusCode::OK);
  }
  assert_eq!(MAX_FIELD_SECTION_SIZE, 64 * 1024);

  // Unread request bodies: drained after the response (up to 64 KiB), or left to a handler
  // that still holds the request.
  let big = "x".repeat(100 * 1024);
  let early = h3
    .request(req(Method::POST, "/early"), Some(&big), None)
    .await;
  if let Ok(early) = early {
    assert_eq!(early.body, "early");
  }
  let small = h3
    .request(req(Method::POST, "/early"), Some("tiny"), None)
    .await
    .unwrap();
  assert_eq!(small.body, "early");
  let kept = h3
    .request(req(Method::POST, "/keep"), Some("held"), None)
    .await
    .unwrap();
  assert_eq!(kept.body, "kept");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_header_timeout_closes_silent_connections_and_streams() {
  let ca = Ca::new();
  let limits = ConnectionLimits::default().header_read_timeout(Duration::from_millis(200));
  let handle = start(&ca, plain_app(), limits).await;

  // A connection that never sends a request is closed.
  let (_endpoint, conn) = quic_connect(handle.local_addr(), quic_client(ca.roots(), false))
    .await
    .unwrap();
  let closed = tokio::time::timeout(WAIT, conn.closed()).await.unwrap();
  assert!(
    matches!(closed, quinn::ConnectionError::ApplicationClosed(ref c) if c.error_code == 0x100u32.into()),
    "{closed:?}"
  );

  // A request stream whose header section never completes is dropped.
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  let (mut send, mut recv) = h3.conn.open_bi().await.unwrap();
  // A HEADERS frame type and a length, then nothing.
  send.write_all(&[0x01, 0x10]).await.unwrap();
  let ended = tokio::time::timeout(WAIT, recv.read_to_end(1024)).await;
  assert!(ended.is_ok(), "the stalled stream was not ended");
  // The connection itself stays usable.
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_requests_per_connection_end_with_goaway() {
  let ca = Ca::new();
  let limits = ConnectionLimits::default().max_requests_per_connection(2);
  let handle = start(&ca, plain_app(), limits).await;
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  // GOAWAY after the second request; once it is done the server closes the connection.
  tokio::time::timeout(WAIT, h3.conn.closed()).await.unwrap();
  assert!(h3.get("/hello").await.is_err());
  // A new connection gets its own budget.
  let mut again = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(again.get("/hello").await.unwrap().body, "hello");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_connection_cap_holds_further_connections() {
  let ca = Ca::new();
  let limits = ConnectionLimits::default().max_connections(1);
  let handle = start(&ca, plain_app(), limits).await;
  let mut first = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(first.get("/hello").await.unwrap().body, "hello");
  // The second connection's handshake completes, but it is not served while the first holds
  // the only slot. (The default 30 s idle timeout: it waits in silence meanwhile.)
  let config = quic_client_with(ca.roots(), false, &QuicLimits::default());
  let mut second = h3_connect_with(handle.local_addr(), config).await.unwrap();
  let waiting = tokio::time::timeout(Duration::from_millis(400), second.get("/hello")).await;
  assert!(waiting.is_err(), "served beyond the connection cap");
  first.conn.close(0u32.into(), b"done");
  drop(first);
  // The freed slot goes to the waiting connection.
  let served = tokio::time::timeout(WAIT, second.get("/hello")).await;
  assert_eq!(served.unwrap().unwrap().body, "hello");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn h3_without_connection_cap_or_header_timeout() {
  let ca = Ca::new();
  let limits = ConnectionLimits::default()
    .dangerous_unlimited_connections()
    .dangerous_no_header_read_timeout();
  let handle = start(&ca, plain_app(), limits).await;
  let mut first = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  let mut second = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(first.get("/hello").await.unwrap().body, "hello");
  assert_eq!(second.get("/hello").await.unwrap().body, "hello");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_h3_ends_when_its_endpoint_closes() {
  let ca = Ca::new();
  let endpoint = QuicEndpoint::bind(
    "127.0.0.1:0".parse().unwrap(),
    QuicEndpointConfig::new(ca.issue().identity()),
  )
  .unwrap();
  let server = {
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
      serve_h3(
        &endpoint,
        plain_app(),
        ConnectionLimits::default(),
        Http3Options::new(),
        std::future::pending(),
      )
      .await
    })
  };
  let mut h3 = h3_connect(endpoint.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  endpoint.close();
  tokio::time::timeout(WAIT, server)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_handshakes_do_not_stop_the_listener() {
  let ca = Ca::new();
  let handle = start(&ca, plain_app(), ConnectionLimits::default()).await;
  // A client that trusts another CA fails the handshake (reported to the route, skipped).
  assert!(
    h3_connect(handle.local_addr(), Ca::new().roots())
      .await
      .is_err()
  );
  let mut h3 = h3_connect(handle.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_h3_on_an_endpoint_of_your_own() {
  let ca = Ca::new();
  let limits = ConnectionLimits::default();
  let endpoint = QuicEndpoint::bind(
    "127.0.0.1:0".parse().unwrap(),
    QuicEndpointConfig::new(ca.issue().identity()).with_limits(quic_limits(&limits)),
  )
  .unwrap();
  let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
  let server = {
    let endpoint = endpoint.clone();
    tokio::spawn(async move {
      serve_h3(&endpoint, plain_app(), limits, Http3Options::new(), async {
        let _ = stopped.await;
      })
      .await
    })
  };
  let mut h3 = h3_connect(endpoint.local_addr(), ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  // The endpoint routes h3 once: a second server on it is refused.
  let again = serve_h3(
    &endpoint,
    plain_app(),
    ConnectionLimits::default(),
    Http3Options::new(),
    std::future::pending(),
  )
  .await;
  assert!(again.is_err());
  drop(h3);
  stop.send(()).unwrap();
  tokio::time::timeout(WAIT, server)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  // The route is released: the endpoint closes h3 connections (`CLOSE_NO_ROUTE`).
  let served = match h3_connect(endpoint.local_addr(), ca.roots()).await {
    Ok(mut h3) => h3.get("/hello").await.is_ok(),
    Err(_) => false,
  };
  assert!(!served, "served after shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_h3_refuses_an_endpoint_that_already_routes_h3() {
  let ca = Ca::new();
  let endpoint = QuicEndpoint::bind(
    "127.0.0.1:0".parse().unwrap(),
    QuicEndpointConfig::new(ca.issue().identity()),
  )
  .unwrap();
  let _taken = endpoint.register(QuicRoute::h3()).unwrap();
  let err = serve_h3(
    &endpoint,
    plain_app(),
    ConnectionLimits::default(),
    Http3Options::new(),
    std::future::pending(),
  )
  .await
  .unwrap_err();
  assert!(err.to_string().starts_with("http3: "), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provided_tls_serves_h3() {
  // F24: settings built from an identity carry it, so the host serves HTTP/3 with it.
  let ca = Ca::new();
  let tls = ServerTls::from_identity(&ca.issue().identity()).unwrap();
  let config = local_config().with_tls(HostTls::provided(tls));
  let handle = ServerHandle::start(&config, plain_app()).await.unwrap();
  let addr = handle.local_addr();
  let (status, headers, _) = tcp_get(addr, ca.roots(), "/hello").await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(
    headers[header::ALT_SVC],
    format!("h3=\":{}\"; ma=86400", addr.port()).as_str()
  );
  let mut h3 = h3_connect(addr, ca.roots()).await.unwrap();
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  handle.shutdown().await;

  // A bare resolver carries no identity: TCP only, as before (and the host logs why).
  let bare = ServerTls::builder(Arc::new(ca.issue().identity()))
    .build()
    .unwrap();
  let config = local_config().with_tls(HostTls::provided(bare));
  let handle = ServerHandle::start(&config, plain_app()).await.unwrap();
  let (status, headers, _) = tcp_get(handle.local_addr(), ca.roots(), "/hello").await;
  assert_eq!(status, StatusCode::OK);
  assert!(headers.get(header::ALT_SVC).is_none());
  assert!(h3_connect(handle.local_addr(), ca.roots()).await.is_err());
  handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_port_taken_fails_the_start() {
  let ca = Ca::new();
  let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
  let port = udp.local_addr().unwrap().port();
  // The TCP port is free (most likely); the UDP one is not.
  let Ok(_tcp) = std::net::TcpListener::bind(("127.0.0.1", port)) else {
    return;
  };
  drop(_tcp);
  let config = HostConfig {
    bind_port: port,
    ..local_config()
  }
  .with_tls(ca.issue().files());
  let err = ServerHandle::start(&config, plain_app()).await.unwrap_err();
  assert!(
    matches!(&err, HostError::Serve(msg) if msg.contains("bind udp")),
    "{err}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_router_serves_http3_until_the_signal() {
  // `serve_router_with_limits` binds UDP too; it runs until SIGINT/SIGTERM, so the test only
  // checks that it serves both listeners and then drops the future.
  let ca = Ca::new();
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  let port = listener.local_addr().unwrap().port();
  drop(listener);
  let env = id_effect_axum::bootstrap_env(id_effect_config::MapConfigProvider::from_pairs([
    ("HOST", "127.0.0.1"),
    ("PORT", port.to_string().as_str()),
  ]));
  let mut host = id_effect_axum::HostBuilder::new()
    .with_env(env)
    .build()
    .await
    .unwrap();
  host.config = host.config.with_tls(ca.issue().files());
  let server = tokio::spawn(serve_router_with_limits(
    host,
    plain_app(),
    ConnectionLimits::default(),
  ));
  let addr: SocketAddr = ([127, 0, 0, 1], port).into();
  let mut h3 = None;
  for _ in 0..50 {
    if let Ok(client) = h3_connect(addr, ca.roots()).await {
      h3 = Some(client);
      break;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  let mut h3 = h3.expect("the host serves HTTP/3");
  assert_eq!(h3.get("/hello").await.unwrap().body, "hello");
  let (_, headers, _) = tcp_get(addr, ca.roots(), "/hello").await;
  assert!(headers.get(header::ALT_SVC).is_some());
  server.abort();
}
