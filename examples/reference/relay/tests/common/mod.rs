//! The Relay test fixture: PostgreSQL in this test binary's own container, three (or more)
//! nodes on real QUIC and TLS on loopback joined into a mesh, signed-in users, and every client
//! the Relay serves — the generated Rust client over HTTP/1.1–2 (TCP) and over HTTP/3 only,
//! signed in with `HttpTransport::with_bearer`, over the RPC WebSocket it dials itself
//! (`wss://`), and WebTransport sessions carrying RPC frames on the host's HTTP/3 endpoint.
//!
//! PostgreSQL tests print `SKIP` without Docker, unless `ID_EFFECT_REQUIRE_DOCKER=1` (or
//! `ID_EFFECT_REQUIRE_PG=1`), which turns a missing Docker into a failure.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use id_effect::{Effect, HNil, Stream};
use id_effect_kit::auth::AuthPrincipal;
use id_effect_kit::axum::run_with_env;
use id_effect_kit::axum::server::security::CookieKey;
use id_effect_kit::axum::server::webtransport::WebTransportStream;
use id_effect_kit::axum::server::webtransport::client::{
  self as wt_client, ConnectError, ConnectRequest, WebTransportClient,
};
use id_effect_kit::config::SecretString;
use id_effect_kit::node::Cookie;
use id_effect_kit::platform::error::HttpError;
use id_effect_kit::platform::http::{
  Http3Client, HttpClient, HttpClientConfig, HttpClientService, HttpRequest, HttpResponse,
  ReqwestHttpClient, StreamingHttpResponse,
};
use id_effect_kit::platform::tls::{NodeCertificate, provider};
use id_effect_kit::rpc::RpcClient;
use id_effect_kit::rpc::channels::ChannelsConfig;
use id_effect_kit::rpc::client::ws::{WsClient, WsClientConfig};
use id_effect_kit::rpc::codec::postcard::{encode_record, split_record};
use id_effect_kit::rpc::service::HttpTransport;
use id_effect_kit::sql::SqlClient;
use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_kit::test::{ContainerError, Postgres, Shared, docker_available};
use relay::api::{RelayClient, RelayService, topic};
use relay::auth::TokenKeys;
use relay::node::{MeshTls, NodeConfig, RelayNode};
use relay::rooms::Rooms;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_rustls::rustls;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// The address every node binds and the client certificate names.
pub const HOST: &str = "127.0.0.1";

/// Generous: the suite runs beside compilations that load every core.
pub const WAIT: Duration = Duration::from_secs(30);

/// The issuer of the test tokens.
pub const ISSUER: &str = "https://id.relay.test";

/// The web client's origin in the tests that do not run a browser.
pub const APP_ORIGIN: &str = "https://chat.relay.test";

/// The test token secret (HS256, 40 bytes).
const SECRET: &[u8] = b"relay-test-token-secret-of-40-bytes!!!!!";

static PG: Shared<Postgres> = Shared::new(Postgres::new);

/// Runs `effect` to its value; a failure is its cause, printed.
pub async fn run<A, E>(effect: Effect<A, E, HNil>) -> Result<A, String>
where
  A: Send + 'static,
  E: std::fmt::Debug + Send + 'static,
{
  run_with_env(HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| format!("{cause:?}"))
}

/// Runs `effect` to its value or its typed failure (a defect or interruption panics).
pub async fn run_typed<A, E>(effect: Effect<A, E, HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: std::fmt::Debug + Send + 'static,
{
  match run_with_env(HNil, move |_| effect).await.into_result() {
    Ok(a) => Ok(a),
    Err(cause) => Err(
      cause
        .fail_option()
        .unwrap_or_else(|| panic!("not a typed failure")),
    ),
  }
}

/// Whether a missing Docker fails the test instead of skipping it.
pub fn docker_required() -> bool {
  ["ID_EFFECT_REQUIRE_DOCKER", "ID_EFFECT_REQUIRE_PG"]
    .iter()
    .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
}

/// The database of this test binary's own PostgreSQL container, with the Relay's schema; `None`
/// (SKIP) without Docker unless it is required.
pub async fn database(test: &str) -> Option<Arc<dyn SqlClient>> {
  let url = postgres_url(test).await?;
  let pool = pg_pool_from_config(PgPoolConfig::from_url(&url).with_max_size(16))
    .await
    .expect("connect to PostgreSQL");
  let db: Arc<dyn SqlClient> = Arc::new(PgSqlClient::new(pool));
  run(Rooms::new(Arc::clone(&db)).migrate())
    .await
    .expect("schema");
  Some(db)
}

/// The URL of this test binary's own PostgreSQL container; `None` (SKIP) without Docker unless
/// it is required.
pub async fn postgres_url(test: &str) -> Option<String> {
  if let Err(e) = tokio::task::spawn_blocking(docker_available)
    .await
    .expect("docker probe")
  {
    assert!(
      !docker_required(),
      "{test}: ID_EFFECT_REQUIRE_DOCKER=1 but Docker is unavailable: {e}"
    );
    eprintln!("SKIP {test}: {e} (set ID_EFFECT_REQUIRE_DOCKER=1 to require it)");
    return None;
  }
  let pg = run(PG.get::<ContainerError, HNil>())
    .await
    .expect("start PostgreSQL");
  Some(pg.url())
}

/// How a test cluster is started.
#[derive(Clone)]
pub struct Options {
  /// How many nodes.
  pub nodes: usize,
  /// The certificate the nodes serve to clients (default: self-signed for [`HOST`]).
  pub certificate: Option<NodeCertificate>,
  /// The web client origins (default: [`APP_ORIGIN`]).
  pub origins: Vec<String>,
  /// Extra CSP `connect-src` sources of the pages (default none).
  pub connect_src: Vec<String>,
  /// The port each node binds (0: a free one).
  pub port: u16,
  /// Advertise this UDP port in `Alt-Svc` instead of the node's own.
  pub advertise_h3_port: Option<u16>,
}

impl Default for Options {
  fn default() -> Self {
    Self {
      nodes: 3,
      certificate: None,
      origins: vec![APP_ORIGIN.to_owned()],
      connect_src: Vec::new(),
      port: 0,
      advertise_h3_port: None,
    }
  }
}

/// Running nodes joined into one mesh.
pub struct Cluster {
  /// The nodes, `a@relay`, `b@relay`, … in order.
  pub nodes: Vec<RelayNode>,
  /// How the nodes verify tokens.
  pub keys: TokenKeys,
  /// The certificate the nodes serve to clients.
  pub certificate: NodeCertificate,
  /// The database.
  pub db: Arc<dyn SqlClient>,
}

/// The name of node `i`: `a@relay`, `b@relay`, …
pub fn node_name(i: usize) -> String {
  let letter = char::from(b'a' + u8::try_from(i).expect("few nodes"));
  format!("{letter}@relay")
}

/// The mesh certificates of `n` nodes, each pinning all of them.
pub fn mesh_pki(n: usize) -> Vec<MeshTls> {
  let certs: Vec<NodeCertificate> = (0..n)
    .map(|_| NodeCertificate::self_signed("cluster").expect("mesh certificate"))
    .collect();
  let trusted: Vec<[u8; 32]> = certs.iter().map(NodeCertificate::cert_sha256).collect();
  certs
    .into_iter()
    .map(|certificate| MeshTls {
      certificate,
      trusted: trusted.clone(),
    })
    .collect()
}

/// The test token keys.
pub fn keys() -> TokenKeys {
  TokenKeys::new(SECRET.to_vec(), ISSUER).expect("keys")
}

/// The configuration of node `i`.
pub fn node_config(
  i: usize,
  db: &Arc<dyn SqlClient>,
  certificate: &NodeCertificate,
  mesh: MeshTls,
  options: &Options,
) -> NodeConfig {
  NodeConfig {
    name: node_name(i),
    bind: SocketAddr::new(HOST.parse().expect("ip"), options.port),
    mesh_bind: SocketAddr::new(HOST.parse().expect("ip"), 0),
    certificate: certificate.clone(),
    mesh,
    cookie: Cookie::new("relay-test-mesh-cookie-of-at-least-32-bytes"),
    origins: options.origins.clone(),
    connect_src: options.connect_src.clone(),
    keys: keys(),
    db: Arc::clone(db),
    cookie_key: CookieKey::generate(),
    channels: ChannelsConfig::new()
      .with_heartbeat_interval(Duration::from_millis(200))
      .with_presence_ttl(Duration::from_secs(2)),
    advertise_h3_port: options.advertise_h3_port,
    enforce: None,
  }
}

/// Starts `options.nodes` nodes and joins every pair (the lower index dials).
pub async fn cluster(db: Arc<dyn SqlClient>, options: Options) -> Cluster {
  let certificate = options
    .certificate
    .clone()
    .unwrap_or_else(|| NodeCertificate::self_signed(HOST).expect("certificate"));
  let mut nodes = Vec::new();
  for (i, mesh) in mesh_pki(options.nodes).into_iter().enumerate() {
    let config = node_config(i, &db, &certificate, mesh, &options);
    nodes.push(RelayNode::start(config).await.expect("node starts"));
  }
  for i in 0..nodes.len() {
    for j in i + 1..nodes.len() {
      let peer = nodes[i]
        .connect(nodes[j].mesh_addr())
        .await
        .expect("mesh join");
      assert_eq!(peer, node_name(j));
    }
  }
  let n = nodes.len();
  eventually("every node has a session with every other", || {
    nodes
      .iter()
      .all(|node| node.connected_peers().len() == n - 1)
  })
  .await;
  Cluster {
    nodes,
    keys: keys(),
    certificate,
    db,
  }
}

/// Polls `done` until it holds, for at most [`WAIT`].
pub async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
  let deadline = tokio::time::Instant::now() + WAIT;
  while !done() {
    assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
    tokio::time::sleep(Duration::from_millis(20)).await;
  }
}

impl Cluster {
  /// A token for `user`, valid for ten minutes.
  pub async fn token(&self, user: &str) -> String {
    token(&self.keys, user).await
  }

  /// `https://127.0.0.1:<port>` of node `i`.
  pub fn url(&self, i: usize) -> String {
    format!("https://{HOST}:{}", self.nodes[i].addr().port())
  }

  /// Waits until every node sees `n` members in `room`.
  pub async fn members_everywhere(&self, room: &str, n: usize) {
    let t = topic(room);
    eventually(&format!("{n} members of {room} on every node"), || {
      self
        .nodes
        .iter()
        .all(|node| node.state().channels().members(&t).len() == n)
    })
    .await;
  }

  /// The generated client of node `i` over HTTP/1.1 or HTTP/2 on TCP, signed in as `user`.
  pub async fn http(&self, i: usize, user: &str) -> RelayClient<HttpTransport> {
    let config = http_config(&self.certificate);
    let tcp = ReqwestHttpClient::from_config(&config);
    let token = self.token(user).await;
    bearer_client(&self.url(i), tcp, &token)
  }

  /// The generated client of node `i` over HTTP/3 only (no `Alt-Svc`, no TCP fallback: a call
  /// that succeeds went over QUIC), signed in as `user`.
  pub async fn h3(&self, i: usize, user: &str) -> RelayClient<HttpTransport> {
    let config = http_config(&self.certificate).http3(true);
    let client = Http3Client::from_config(&config);
    assert!(client.http3_available());
    let token = self.token(user).await;
    let only = Http3Only {
      client,
      port: self.nodes[i].addr().port(),
    };
    bearer_client(&self.url(i), only, &token)
  }

  /// The generated client of node `i` over the RPC WebSocket (`wss://`), signed in as `user`.
  pub async fn ws(&self, i: usize, user: &str) -> RelayClient<WsClient> {
    let token = self.token(user).await;
    let socket = wss(self.nodes[i].addr(), &self.certificate, &token)
      .await
      .expect("websocket");
    RelayService::client(socket)
  }

  /// A WebTransport session with node `i`, signed in as `user`.
  pub async fn wt(&self, i: usize, user: &str) -> Wt {
    let token = self.token(user).await;
    Wt::connect(self.nodes[i].addr(), &self.certificate, Some(&token))
      .await
      .expect("webtransport session")
  }

  /// Stops every node.
  pub async fn shutdown(&self) {
    for node in &self.nodes {
      node.shutdown().await;
    }
  }
}

/// A token for `user` from `keys`, valid for ten minutes.
pub async fn token(keys: &TokenKeys, user: &str) -> String {
  run(
    keys
      .issuer(Duration::from_secs(600))
      .issue(&AuthPrincipal::new(user)),
  )
  .await
  .expect("token")
}

/// The certificate to trust for `certificate`: the last of its chain (the CA of an `id-effect dev`
/// certificate, the certificate itself when it is self-signed).
pub fn anchor(certificate: &NodeCertificate) -> &rustls::pki_types::CertificateDer<'static> {
  certificate.chain().last().expect("a certificate")
}

/// The platform HTTP client settings that trust `certificate` and may reach loopback.
pub fn http_config(certificate: &NodeCertificate) -> HttpClientConfig {
  let mut config = HttpClientConfig::default()
    .add_root_certificate_der(anchor(certificate).to_vec())
    // The SSRF guard refuses loopback unless the host is exempt.
    .allow_hosts([HOST])
    .with_timeout(Duration::from_secs(15));
  // A debug build on a busy machine may need more than the default 300 ms handshake budget.
  config.http3.fallback_timeout = Duration::from_secs(5);
  config
}

/// The platform's HTTP/3 client sending every request over HTTP/3 to one port.
pub struct Http3Only {
  /// The client.
  pub client: Http3Client,
  /// The UDP port.
  pub port: u16,
}

impl HttpClient for Http3Only {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    self.client.execute_h3(req, self.port)
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    self.client.execute_stream(req)
  }
}

/// The generated client of `url` over `http`, anonymous.
pub fn http_client(url: &str, http: impl HttpClient) -> RelayClient<HttpTransport> {
  RelayService::client(transport(url, http))
}

/// The generated client of `url` over `http`, signed in with `token`: the transport's own
/// bearer header (F56, `HttpTransport::with_bearer`; the token stays a `Secret`).
pub fn bearer_client(url: &str, http: impl HttpClient, token: &str) -> RelayClient<HttpTransport> {
  let transport = transport(url, http)
    .with_bearer(SecretString::new(token.to_owned()))
    .expect("a token is a valid header value");
  RelayService::client(transport)
}

fn transport(url: &str, http: impl HttpClient) -> HttpTransport {
  HttpTransport::new(RpcClient::new(url), HttpClientService::new(http))
}

/// A rustls client config trusting only `certificate` (TLS 1.3).
pub fn client_tls(certificate: &NodeCertificate) -> rustls::ClientConfig {
  let mut roots = rustls::RootCertStore::empty();
  roots
    .add(anchor(certificate).clone())
    .expect("root certificate");
  rustls::ClientConfig::builder_with_provider(provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls 1.3")
    .with_root_certificates(roots)
    .with_no_client_auth()
}

/// The RPC WebSocket of the node at `addr`, dialled by the Rust client itself over `wss://`
/// (F40: rustls with the platform roots plus `certificate`'s anchor, the server name checked, no
/// way to accept an untrusted certificate), the upgrade carrying the user's bearer token.
pub async fn wss(
  addr: SocketAddr,
  certificate: &NodeCertificate,
  token: &str,
) -> Result<WsClient, String> {
  let mut request = format!("wss://{HOST}:{}/rpc/ws", addr.port())
    .into_client_request()
    .map_err(|e| e.to_string())?;
  let authorization = format!("Bearer {token}");
  request.headers_mut().insert(
    "authorization",
    HeaderValue::from_str(&authorization).map_err(|e| e.to_string())?,
  );
  let config = WsClientConfig::new().add_root_certificate_der(anchor(certificate).to_vec());
  WsClient::connect_with(request, &config)
    .await
    .map_err(|e| e.to_string())
}

/// Pumps `stream` into a channel on its own task (aborting the task drops the stream: the call
/// is cancelled). A failure arrives as `Err` and ends the channel.
pub fn pump<O, E>(
  stream: Stream<O, E, HNil>,
) -> (
  tokio::sync::mpsc::UnboundedReceiver<Result<O, String>>,
  tokio::task::JoinHandle<()>,
)
where
  O: Send + 'static,
  E: std::fmt::Debug + Send + 'static,
{
  let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
  // One item per pull: a live stream must not wait to fill a chunk.
  let task = tokio::spawn(async move {
    let mut stream = stream;
    loop {
      match run(stream.next_chunk_with_size(1)).await {
        Ok(Some((chunk, rest))) => {
          for item in chunk.into_vec() {
            let _ = tx.send(Ok(item));
          }
          stream = rest;
        }
        Ok(None) => break,
        Err(e) => {
          let _ = tx.send(Err(e));
          break;
        }
      }
    }
  });
  (rx, task)
}

/// The next value of a pumped stream, within [`WAIT`].
pub async fn next<O>(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Result<O, String>>) -> O {
  tokio::time::timeout(WAIT, rx.recv())
    .await
    .expect("an item in time")
    .expect("the stream is open")
    .expect("the stream did not fail")
}

/// One WebTransport session carrying RPC frames: a control stream, then one stream per call.
pub struct Wt {
  session: WebTransportClient,
  _control: WebTransportStream,
  ids: AtomicU64,
  /// The server's `hello` (session, window, …).
  pub hello: Value,
}

impl Wt {
  /// Opens a session with the node at `addr` (`/rpc/wt`), with a bearer token when given, and
  /// reads the `hello` on its control stream.
  ///
  /// # Errors
  ///
  /// The CONNECT was refused (its status) or the connection failed.
  pub async fn connect(
    addr: SocketAddr,
    certificate: &NodeCertificate,
    token: Option<&str>,
  ) -> Result<Self, ConnectError> {
    let request = ConnectRequest {
      origin: None,
      headers: token
        .map(|t| vec![("authorization".to_owned(), format!("Bearer {t}"))])
        .unwrap_or_default(),
    };
    let session =
      wt_client::connect(addr, HOST, client_tls(certificate), "/rpc/wt", &request).await?;
    let mut control = Frames::new(session.open_bi().await?);
    control.send(&json!({"id": 0, "kind": "ping"})).await;
    let hello = control.next().await.expect("a hello on the control stream");
    assert_eq!(hello["kind"], "hello", "{hello}");
    Ok(Self {
      session,
      _control: control.stream,
      ids: AtomicU64::new(1),
      hello: hello["payload"].clone(),
    })
  }

  /// Opens a call of `tag` with `payload` on a stream of its own.
  pub async fn call(&self, tag: &str, payload: Value) -> WtCall {
    let id = self.ids.fetch_add(1, Ordering::Relaxed);
    let stream = self.session.open_bi().await.expect("stream");
    let mut frames = Frames::new(stream);
    frames
      .send(&json!({"id": id, "kind": "call", "tag": tag, "v": 1, "payload": payload}))
      .await;
    WtCall { id, frames }
  }

  /// Ends the session.
  pub fn close(&self) {
    self.session.close(0, "done");
  }
}

/// One call on a WebTransport session.
pub struct WtCall {
  id: u64,
  frames: Frames,
}

impl WtCall {
  /// Sends one client item.
  pub async fn send(&mut self, payload: Value) {
    let frame = json!({"id": self.id, "kind": "item", "payload": payload});
    self.frames.send(&frame).await;
  }

  /// The next item's payload (credit frames skipped); panics on an exit or the end.
  pub async fn item(&mut self) -> Value {
    loop {
      let frame = self.frames.next().await.expect("a frame");
      match frame["kind"].as_str() {
        Some("credit") => {}
        Some("item") => return frame["payload"].clone(),
        _ => panic!("expected an item: {frame}"),
      }
    }
  }

  /// The next frame that is not a credit (an item or the exit), or `None` at the end.
  pub async fn frame(&mut self) -> Option<Value> {
    loop {
      let frame = self.frames.next().await?;
      if frame["kind"] != "credit" {
        return Some(frame);
      }
    }
  }
}

/// The u32-BE length-prefixed JSON records of one WebTransport stream.
pub struct Frames {
  stream: WebTransportStream,
  buf: Vec<u8>,
}

impl Frames {
  fn new(stream: WebTransportStream) -> Self {
    Self {
      stream,
      buf: Vec::new(),
    }
  }

  async fn send(&mut self, frame: &Value) {
    let record = encode_record(frame.to_string().as_bytes()).expect("record");
    self.stream.send.write_all(&record).await.expect("write");
  }

  async fn next(&mut self) -> Option<Value> {
    loop {
      if let Some((record, used)) = split_record(&self.buf).expect("record") {
        let frame = serde_json::from_slice(record).expect("json frame");
        self.buf.drain(..used);
        return Some(frame);
      }
      let mut chunk = [0_u8; 4096];
      let n = tokio::time::timeout(WAIT, self.stream.recv.read(&mut chunk))
        .await
        .ok()?
        .ok()?;
      if n == 0 {
        return None;
      }
      self.buf.extend_from_slice(&chunk[..n]);
    }
  }
}
