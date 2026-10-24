//! One Relay node: clients on one port, served by the host entry point, and the mesh on a QUIC
//! endpoint of its own.
//!
//! - **Clients**, one port number served by the host ([`ServerHandle`], ADR 0014 D7 for
//!   clients): TLS on TCP with HTTP/2 and HTTP/1.1 (RPC over HTTP, the RPC WebSocket — over
//!   HTTP/1.1 upgrades and RFC 8441 extended CONNECT on HTTP/2 —, the web client) and, on UDP,
//!   the host's HTTP/3 endpoint ([`HostConfig::http3`]) taking WebTransport sessions that carry
//!   RPC streams ([`HostConfig::webtransport`]). Every TCP response carries
//!   `Alt-Svc: h3=":<port>"`, so HTTP/3 clients upgrade and fall back to TCP when UDP is blocked.
//!   No QUIC endpoint of the node's own serves HTTP/3.
//! - **The mesh**: a QUIC endpoint of its own ([`NodeConfig::mesh_bind`]) serving `idm/1` only,
//!   with mutual TLS (a client certificate is required and must be one of the pinned node
//!   certificates). The host does not hand out its endpoint, so the mesh cannot ride the client
//!   port.
//!
//! Over the mesh the node runs a channels hub (each room is a topic; presence is shared by the
//! hubs) and a mesh RPC endpoint ([`RelayPeer`](crate::api::RelayPeer)); each session to another
//! node runs on a node session pump that verifies every envelope before it is delivered, and
//! carries both ([`attach_session_rpc`]).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use axum::response::IntoResponse;
use id_effect::DynEnv;
use id_effect_kit::axum::realtime::ws::WsConfig;
use id_effect_kit::axum::route::{self, AccessLayer, RouteAccess, RouteInfo};
use id_effect_kit::axum::server::h3::Http3Options;
use id_effect_kit::axum::server::security::{ContentSecurityPolicy, CookieKey, SecurityLayer};
use id_effect_kit::axum::server::webtransport::WebTransportConfig;
use id_effect_kit::axum::server::{AltSvcLayer, ConnectionLimits, HostTls, ServerTls};
use id_effect_kit::axum::{
  BodyLimitLayer, HostConfig, HostError, ReadinessState, RequestIdLayer, ServerHandle, TimeoutLayer,
};
use id_effect_kit::node::transport::quic::{QuicConfig, QuicTransport};
use id_effect_kit::node::{
  BehaviorRegistry, Cookie, DeliveryConfig, Listener, Mesh, NodeId, Session, SessionPumpStats,
  Transport, TransportError,
};
use id_effect_kit::platform::quic::{QuicEndpoint, QuicEndpointConfig};
use id_effect_kit::platform::tls::{Identity, NodeCertificate};
use id_effect_kit::posture::{Posture, Surface};
use id_effect_kit::process::{NodeName, ProcessNode};
use id_effect_kit::rpc::RpcServer;
use id_effect_kit::rpc::channels::{
  Channels, ChannelsConfig, HeartbeatTask, SessionLink, attach_session_rpc, register_hub,
};
use id_effect_kit::rpc::subscription::TailConfig;
use id_effect_kit::rpc::subscription::events::EventSubscription;
use id_effect_kit::rpc::transport::mesh::{MeshEndpoint, MeshRpc};
use id_effect_kit::rpc::transport::webtransport::{self, RpcWebTransportConfig};
use id_effect_kit::rpc::transport::ws::RpcWsConfig;
use id_effect_kit::sql::SqlClient;

use crate::api::{ChatMessage, RelayPeerService, RelayService};
use crate::auth::{TokenKeys, rpc_principal, token_from_query};
use crate::handlers::{NodeState, Peers, Relays, peer_service};
use crate::idempotency::PgIdempotencyStore;
use crate::rooms::{MembersOnly, Rooms};

/// Where the RPC routes are mounted (`/rpc`, `/rpc/ws`, `/rpc/wt`).
pub const RPC_PATH: &str = "/rpc";

/// How long a request may run before it is answered `503` (streams on WebSocket and
/// WebTransport run on their own connection, past the upgrade).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest request body accepted.
pub const BODY_LIMIT: usize = 1024 * 1024;

/// How often the history tail polls the event log for new messages.
pub const HISTORY_POLL: Duration = Duration::from_millis(50);

/// How many times [`RelayNode::start`] tries a fresh port when asked for port 0 and the UDP port
/// matching the TCP one the host bound is taken.
const BIND_ATTEMPTS: usize = 16;

/// The web client page.
const INDEX_HTML: &str = include_str!("../web/index.html");

/// The web client script (served from the origin: the CSP admits `script-src 'self'`).
const APP_JS: &str = include_str!("../web/app.js");

/// Why a node did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError(pub String);

impl std::fmt::Display for StartError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "relay: {}", self.0)
  }
}

impl std::error::Error for StartError {}

fn start_error(what: &str, e: impl std::fmt::Display) -> StartError {
  StartError(format!("{what}: {e}"))
}

/// The mesh's TLS: this node's certificate and the certificates of every node it accepts
/// (pinned by SHA-256, in both directions: servers and clients).
#[derive(Clone)]
pub struct MeshTls {
  /// This node's certificate (its name is [`QuicConfig::server_name`], `cluster` by default).
  pub certificate: NodeCertificate,
  /// The SHA-256 of every node certificate accepted (this node's own included).
  pub trusted: Vec<[u8; 32]>,
}

impl std::fmt::Debug for MeshTls {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MeshTls")
      .field("trusted", &self.trusted.len())
      .finish_non_exhaustive()
  }
}

impl MeshTls {
  fn quic_config(&self) -> Result<QuicConfig, StartError> {
    let mut config = QuicConfig::from_certificate(&self.certificate)
      .map_err(|e| start_error("mesh certificate", e))?;
    for pin in &self.trusted {
      config = config.with_pinned_peer_cert_sha256(*pin);
    }
    Ok(config)
  }
}

/// What a node is started with.
#[derive(Clone)]
pub struct NodeConfig {
  /// The node's name (`name@host`, unique in the cluster).
  pub name: String,
  /// The clients' address, served by the host: TLS on TCP and HTTP/3 with WebTransport on UDP,
  /// one port number (port 0 picks a free one).
  pub bind: SocketAddr,
  /// The mesh's address: UDP, `idm/1` only, on a QUIC endpoint of its own (port 0 picks a free
  /// one). Peers [`connect`](RelayNode::connect) to it.
  pub mesh_bind: SocketAddr,
  /// The certificate served to clients on TCP and HTTP/3 (a real one in production: the prod
  /// posture refuses the `id-effect dev` certificate).
  pub certificate: NodeCertificate,
  /// The mesh's TLS.
  pub mesh: MeshTls,
  /// The mesh's shared cookie (at least 32 bytes).
  pub cookie: Cookie,
  /// The origins whose pages may open the RPC WebSocket and WebTransport sessions (the web
  /// client's origin); none: browsers are refused.
  pub origins: Vec<String>,
  /// Sources the web client's CSP `connect-src` admits beside `'self'` (the node's own origin:
  /// its HTTPS, WebSocket and WebTransport URLs, one port for TCP and UDP). List the WebTransport
  /// and WebSocket origins of anything else the page is pointed at with `#wt=` / `#ws=` (another
  /// node, a UDP load balancer on another port: `https://chat.example:4433`): CSP matches scheme,
  /// host and port, so the browser refuses any other. None by default.
  pub connect_src: Vec<String>,
  /// How bearer tokens are verified.
  pub keys: TokenKeys,
  /// The database: rooms, members and history.
  pub db: Arc<dyn SqlClient>,
  /// The key signing the web client's cookies.
  pub cookie_key: CookieKey,
  /// Channel limits and timings (heartbeat, presence TTL).
  pub channels: ChannelsConfig,
  /// The UDP port `Alt-Svc` advertises when it is not the node's own (a UDP load balancer in
  /// front); `None`: the node's port.
  pub advertise_h3_port: Option<u16>,
  /// The startup posture check (the profile and its accepted risks): when set, a node whose
  /// posture fails under it refuses to start, before it binds a socket.
  pub enforce: Option<Posture>,
}

impl std::fmt::Debug for NodeConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NodeConfig")
      .field("name", &self.name)
      .field("bind", &self.bind)
      .field("mesh_bind", &self.mesh_bind)
      .field("origins", &self.origins)
      .field("connect_src", &self.connect_src)
      .finish_non_exhaustive()
  }
}

/// The settings the posture check reads off a running node.
#[derive(Clone)]
struct Surfaces {
  ws: RpcWsConfig,
  wt: WebTransportConfig,
  h3: Http3Options,
  host: HostConfig,
  retry: bool,
  certificate: Vec<u8>,
}

impl Surfaces {
  fn posture(&self, base: Posture) -> Posture {
    stated(base)
      .rpc_websocket(&self.ws)
      .webtransport(&self.wt)
      // HTTP/3 with WebTransport is the host's (`server.http3`), with the host's certificate.
      .host(&self.host)
      .http3_options(&self.h3)
      // Both QUIC endpoints, the host's and the mesh's, validate client addresses.
      .quic_retry(self.retry)
      .tls_certificate(&self.certificate)
  }
}

/// A running node.
pub struct RelayNode {
  id: NodeId,
  addr: SocketAddr,
  mesh_addr: SocketAddr,
  state: Arc<NodeState>,
  rpc: MeshRpc,
  mesh_endpoint: QuicEndpoint,
  dialer: QuicTransport,
  cookie: Cookie,
  links: Arc<Mutex<Vec<SessionLink>>>,
  host: ServerHandle,
  host_config: HostConfig,
  surfaces: Surfaces,
  _peer_endpoint: MeshEndpoint,
  _heartbeat: HeartbeatTask,
  _accept: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for RelayNode {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RelayNode")
      .field("name", &self.state.node())
      .field("addr", &self.addr)
      .field("mesh_addr", &self.mesh_addr)
      .finish_non_exhaustive()
  }
}

impl RelayNode {
  /// Starts a node (inside a tokio runtime): the mesh, the channels hub and the mesh RPC
  /// endpoint, the host ([`ServerHandle`]: TLS on TCP and HTTP/3 with WebTransport on the same
  /// port number), then the mesh's own QUIC endpoint (`idm/1`). Peers are joined with
  /// [`connect`](Self::connect).
  ///
  /// # Errors
  ///
  /// [`StartError`] when a certificate, the mesh, a port or a listener fails.
  pub async fn start(config: NodeConfig) -> Result<Self, StartError> {
    let name = NodeName::new(config.name.clone());
    let id = NodeId::fresh(&name);
    let mut registry = BehaviorRegistry::new();
    register_hub(&mut registry);
    MeshRpc::register_behavior(&mut registry);
    let mesh = Mesh::new(id.clone(), ProcessNode::new(name), registry)
      .with_delivery(DeliveryConfig::at_least_once());
    let rpc = MeshRpc::new(id.clone(), mesh).map_err(|e| start_error("mesh rpc", e))?;
    let rooms = Rooms::new(Arc::clone(&config.db));
    let channels = Channels::<ChatMessage>::builder()
      .config(config.channels.clone())
      .join_policy(MembersOnly::new(rooms.clone()))
      .start_on_mesh(rpc.mesh())
      .map_err(|e| start_error("channels", e))?;
    let heartbeat = channels.spawn_heartbeat();
    let history = EventSubscription::new(rooms.journal())
      .with_config(TailConfig::new().with_poll_interval(HISTORY_POLL));
    let state = Arc::new(NodeState::new(
      config.name.clone(),
      rooms,
      history,
      channels,
      rpc.clone(),
    ));
    let peer_endpoint = rpc.serve(
      peer_service(&config.name),
      RelayPeerService::group(Peers(Arc::clone(&state))),
      DynEnv::new(),
    );
    peer_endpoint
      .start()
      .map_err(|e| start_error("mesh endpoint", e))?;

    let (app, ws, wt_config) = app(&state, &config)?;
    let identity =
      Identity::from_certificate(&config.certificate).map_err(|e| start_error("tls", e))?;
    let tls = ServerTls::from_identity(&identity).map_err(|e| start_error("tls", e))?;
    // The host serves every client surface: TLS on TCP, and HTTP/3 on UDP with the same port
    // number taking the WebTransport sessions of `/rpc/wt` (F43). The provided TLS settings
    // carry the certificate identity QUIC presents.
    let host = HostConfig {
      bind_host: config.bind.ip().to_string(),
      bind_port: config.bind.port(),
      http3: true,
      ..HostConfig::default()
    }
    .with_tls(HostTls::provided(tls))
    .with_webtransport();
    let limits = ConnectionLimits::default();
    // What the host's HTTP/3 endpoint runs with: the host builds it from the same identity and
    // serves it with `Http3Options::default().webtransport()`.
    let host_quic = QuicEndpointConfig::new(identity);
    let h3 = Http3Options::default().webtransport();
    let mesh_identity = Identity::from_certificate(&config.mesh.certificate)
      .map_err(|e| start_error("mesh certificate", e))?;
    let mesh_quic = QuicEndpointConfig::new(mesh_identity);
    let surfaces = Surfaces {
      ws,
      wt: wt_config,
      h3,
      host: host.clone(),
      retry: host_quic.retry() && mesh_quic.retry(),
      certificate: config
        .certificate
        .chain()
        .first()
        .map(|c| c.to_vec())
        .unwrap_or_default(),
    };
    // The startup gate (ADR 0013 R10): checked before any socket is bound.
    if let Some(base) = config.enforce.clone() {
      surfaces
        .posture(base)
        .check()
        .enforce()
        .map_err(|refused| start_error("posture", refused))?;
    }
    let (server, host) = bind(&config, host, &limits, app).await?;
    let addr = server.local_addr();

    let (mesh_endpoint, listener, dialer) = match mesh_transport(&config, mesh_quic).await {
      Ok(mesh) => mesh,
      Err(e) => {
        server.shutdown().await;
        return Err(e);
      }
    };
    let mesh_addr = mesh_endpoint.local_addr();
    let links = Arc::new(Mutex::new(Vec::new()));
    let accept = tokio::spawn(accept_loop(
      listener,
      id.clone(),
      config.cookie.clone(),
      rpc.clone(),
      Arc::clone(&state),
      Arc::clone(&links),
    ));
    Ok(Self {
      id,
      addr,
      mesh_addr,
      state,
      rpc,
      mesh_endpoint,
      dialer,
      cookie: config.cookie,
      links,
      host: server,
      host_config: host,
      surfaces,
      _peer_endpoint: peer_endpoint,
      _heartbeat: heartbeat,
      _accept: accept,
    })
  }

  /// The node's name.
  pub fn name(&self) -> &str {
    self.state.node()
  }

  /// The clients' address, served by the host: TLS on TCP and HTTP/3 with WebTransport on UDP,
  /// this one port number.
  pub fn addr(&self) -> SocketAddr {
    self.addr
  }

  /// The mesh's address (UDP, `idm/1`): what peers [`connect`](Self::connect) to.
  pub fn mesh_addr(&self) -> SocketAddr {
    self.mesh_addr
  }

  /// The host settings the node serves its clients with: HTTP/3 on, WebTransport on, TLS on
  /// TCP and UDP from the node's certificate.
  pub fn host_config(&self) -> &HostConfig {
    &self.host_config
  }

  /// The node's state (channels, history readers), for operators and tests.
  pub fn state(&self) -> &Arc<NodeState> {
    &self.state
  }

  /// The node's mesh RPC.
  pub fn mesh(&self) -> &MeshRpc {
    &self.rpc
  }

  /// The mesh's QUIC endpoint (statistics: accepted connections, refused client certificates).
  pub fn mesh_endpoint(&self) -> &QuicEndpoint {
    &self.mesh_endpoint
  }

  /// What the node session pump of each open session did with the envelopes its peer sent
  /// (delivered, refused by the mesh, dropped as unverified), by peer name.
  pub fn sessions(&self) -> Vec<(String, SessionPumpStats)> {
    let mut sessions: Vec<(String, SessionPumpStats)> = self
      .links
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .filter(|l| l.is_open())
      .map(|l| (l.peer().to_owned(), l.stats()))
      .collect();
    sessions.sort_by(|a, b| a.0.cmp(&b.0));
    sessions
  }

  /// Joins the node whose mesh listens at `peer` (its [`mesh_addr`](Self::mesh_addr)): a QUIC
  /// connection on `idm/1` from this node's mesh endpoint, mutual TLS, the cookie handshake,
  /// then one session on a node session pump carrying the channels and mesh RPC.
  ///
  /// # Errors
  ///
  /// [`StartError`] when the dial, the TLS handshake or the session handshake fails.
  pub async fn connect(&self, peer: SocketAddr) -> Result<String, StartError> {
    let conn = self
      .dialer
      .connect(&peer.to_string())
      .await
      .map_err(|e| start_error("mesh dial", e))?;
    let session = Session::initiate(conn, &self.id, &self.cookie)
      .await
      .map_err(|e| start_error("mesh session", e))?;
    let link = attach_session_rpc(&self.rpc, session);
    let name = link.peer().to_owned();
    self.state.add_peer(&name);
    self
      .links
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(link);
    Ok(name)
  }

  /// The names of the peers with an open session.
  pub fn connected_peers(&self) -> Vec<String> {
    let mut peers: Vec<String> = self
      .links
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .filter(|l| l.is_open())
      .map(|l| l.peer().to_owned())
      .collect();
    peers.sort();
    peers.dedup();
    peers
  }

  /// What this node states about itself for the startup posture check (ADR 0013 R10), on top
  /// of `base` (the profile and its accepted risks): no console, no `/docs`, the mesh on QUIC
  /// with mutual TLS, CSRF on, every procedure of both services declared, the realtime origin
  /// allowlists, the host's HTTP/3 with TLS, 0-RTT off, Retry on, the served certificate.
  pub fn posture(&self, base: Posture) -> Posture {
    self.surfaces.posture(base)
  }

  /// Stops the node: the rooms' members leave, the mesh endpoint closes (peers see the node go
  /// down at once), then the host drains TCP and HTTP/3.
  pub async fn shutdown(&self) {
    self.state.channels().shutdown();
    self.mesh_endpoint.close();
    self
      .links
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();
    self.host.shutdown().await;
  }
}

/// The procedures and posture statements that do not depend on a running node.
pub fn stated(base: Posture) -> Posture {
  base
    .console(Surface::Absent)
    .docs(Surface::Absent)
    .mesh_encrypted(true)
    .csrf(true)
    .rpc_service(&RelayService::contract())
    .rpc_service(&RelayPeerService::contract())
}

/// Accepts mesh connections on `idm/1` for the life of the node: each one's session handshake
/// runs on its own task, so a slow peer holds up nobody.
async fn accept_loop(
  listener: Box<dyn Listener>,
  id: NodeId,
  cookie: Cookie,
  rpc: MeshRpc,
  state: Arc<NodeState>,
  links: Arc<Mutex<Vec<SessionLink>>>,
) {
  loop {
    match listener.accept().await {
      Ok(conn) => {
        let (id, cookie, rpc) = (id.clone(), cookie.clone(), rpc.clone());
        let (state, links) = (Arc::clone(&state), Arc::clone(&links));
        tokio::spawn(async move {
          match Session::respond(conn, &id, &cookie).await {
            Ok(session) => {
              let link = attach_session_rpc(&rpc, session);
              state.add_peer(link.peer());
              links
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(link);
            }
            Err(e) => eprintln!("relay: mesh session refused: {e}"),
          }
        });
      }
      Err(TransportError::Closed) => break,
      // A refused handshake (no or an unknown client certificate): logged by the endpoint.
      Err(_) => {}
    }
  }
}

/// Starts the host on one port number, TLS on TCP and HTTP/3 with WebTransport on UDP: the
/// configured port, or (port 0) a free TCP port whose UDP twin is free too. Returns the running
/// host and its settings with the bound port.
async fn bind(
  config: &NodeConfig,
  host: HostConfig,
  limits: &ConnectionLimits,
  app: axum::Router,
) -> Result<(ServerHandle, HostConfig), StartError> {
  let attempts = if config.bind.port() == 0 {
    BIND_ATTEMPTS
  } else {
    1
  };
  // Another UDP port in `Alt-Svc` (a UDP load balancer in front): set on the response before
  // the host's own layer, which keeps a header that is already there.
  let app = match config.advertise_h3_port {
    Some(port) => app.layer(AltSvcLayer::h3(port)),
    None => app,
  };
  let mut last = StartError(String::new());
  for _ in 0..attempts {
    match ServerHandle::start_with_limits(
      &host,
      app.clone(),
      ReadinessState::new(true),
      limits.clone(),
    )
    .await
    {
      Ok(server) => {
        let host = HostConfig {
          bind_port: server.local_addr().port(),
          ..host
        };
        return Ok((server, host));
      }
      Err(e) => last = host_error(&e),
    }
  }
  Err(last)
}

/// A host that did not start: `quic: …` when its HTTP/3 endpoint could not bind the UDP port,
/// `tcp: …` otherwise.
fn host_error(e: &HostError) -> StartError {
  let why = e.to_string();
  if why.contains("bind udp") {
    start_error("quic", why)
  } else {
    start_error("tcp", why)
  }
}

/// The mesh's own QUIC endpoint on [`NodeConfig::mesh_bind`], its `idm/1` listener (mutual TLS
/// against the pinned node certificates) and the dialer leaving from the same endpoint.
async fn mesh_transport(
  config: &NodeConfig,
  quic: QuicEndpointConfig,
) -> Result<(QuicEndpoint, Box<dyn Listener>, QuicTransport), StartError> {
  let endpoint =
    QuicEndpoint::bind(config.mesh_bind, quic).map_err(|e| start_error("mesh endpoint", e))?;
  let ready = async {
    let listener = QuicTransport::on_endpoint(config.mesh.quic_config()?, &endpoint)
      .listen("")
      .await
      .map_err(|e| start_error("mesh listener", e))?;
    let dialer = QuicTransport::on_endpoint(config.mesh.quic_config()?, &endpoint);
    Ok::<_, StartError>((listener, dialer))
  }
  .await;
  match ready {
    Ok((listener, dialer)) => Ok((endpoint, listener, dialer)),
    Err(e) => {
      endpoint.close();
      Err(e)
    }
  }
}

/// The plain routes next to the RPC routes, each declared public: the web client and the health
/// check. The deny-by-default layer answers `404` for anything else.
pub fn plain_routes() -> [RouteInfo; 3] {
  let public = |path: &str| {
    let mut info = RouteInfo::new(Method::GET, path);
    info.access = RouteAccess::Public;
    info
  };
  [public("/"), public("/app.js"), public("/health")]
}

/// The application of one node: the RPC routes (HTTP, WebSocket, WebTransport) behind bearer
/// authentication, the web client and `/health`, with the security headers, CSRF protection
/// (`/rpc` is exempt: every RPC POST needs `x-rpc: 1` and a codec content type, ADR 0014 C5),
/// the body limit, the request timeout and request ids.
fn app(
  state: &Arc<NodeState>,
  config: &NodeConfig,
) -> Result<(axum::Router, RpcWsConfig, WebTransportConfig), StartError> {
  let socket = WsConfig::default()
    .allowed_origins(config.origins.iter().cloned())
    .map_err(|e| start_error("origins", e))?
    // A chat client sends a frame per message and credits back per item read.
    .messages_per_second(1_000);
  // One RpcWsConfig for both transports: one resume store, so a session resumes across them.
  let ws = RpcWsConfig::default().socket(socket);
  let session = WebTransportConfig::default()
    .allowed_origins(config.origins.iter().cloned())
    .map_err(|e| start_error("origins", e))?;
  let server = RpcServer::new(RelayService::group(Relays(Arc::clone(state))))
    .with_websocket(ws.clone())
    .with_principal(rpc_principal)
    // A retried `Relay.send` with the same `Idempotency-Key` posts once, on any node.
    .with_idempotency_store(PgIdempotencyStore::new(Arc::clone(&config.db)));
  let wt_route = webtransport::endpoint(
    server.clone(),
    RpcWebTransportConfig::default()
      .session(session.clone())
      .rpc(ws.clone()),
  );
  // `POST /rpc`, `POST|GET /rpc/{tag}`, `GET /rpc/ws`, `CONNECT /rpc/wt`.
  let rpc = server.mount(route::Router::new()).route(wt_route);
  let access = AccessLayer::new(rpc.routes().chain(&plain_routes()));
  let security = SecurityLayer::recommended(config.cookie_key.clone())
    .csrf_exempt(RPC_PATH)
    .csp(content_security_policy(&config.connect_src))
    .map_err(|e| start_error("connect_src", e))?;
  let router = axum::Router::new()
    .merge(rpc.with_state(DynEnv::new()).into_axum())
    .route("/", axum::routing::get(index))
    .route("/app.js", axum::routing::get(script))
    .route("/health", axum::routing::get(|| async { "ok" }))
    .route_layer(access)
    .layer(config.keys.layer())
    .layer(axum::middleware::from_fn(token_from_query))
    .layer(security)
    .layer(BodyLimitLayer::new(BODY_LIMIT))
    .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
    .layer(RequestIdLayer::new());
  Ok((router, ws, session))
}

/// The pages' CSP: [`ContentSecurityPolicy::recommended`] with `connect-src 'self'` and the
/// configured `connect_src` sources (the WebTransport and WebSocket URLs the web client may be
/// pointed at besides the node's own origin).
pub fn content_security_policy(connect_src: &[String]) -> ContentSecurityPolicy {
  let mut sources = vec!["'self'"];
  sources.extend(connect_src.iter().map(String::as_str));
  ContentSecurityPolicy::recommended().connect_src(&sources)
}

async fn index() -> impl IntoResponse {
  (
    StatusCode::OK,
    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
    INDEX_HTML,
  )
}

async fn script() -> impl IntoResponse {
  (
    StatusCode::OK,
    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
    APP_JS,
  )
}
