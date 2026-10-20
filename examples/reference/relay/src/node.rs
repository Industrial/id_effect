//! One Relay node: one port for everything (ADR 0014 D7).
//!
//! - **UDP**: one platform QUIC endpoint routing by ALPN — `h3` (HTTP/3, and WebTransport
//!   sessions carrying RPC streams) and `idm/1` (the mesh, mutual TLS: a client certificate is
//!   required and must be one of the pinned node certificates).
//! - **TCP**, the same port number: TLS with HTTP/2 and HTTP/1.1 (RPC over HTTP, the RPC
//!   WebSocket, the web client), every response carrying `Alt-Svc: h3=":<port>"` so HTTP/3
//!   clients upgrade and fall back to TCP when UDP is blocked.
//!
//! Over the mesh the node runs a channels hub (each room is a topic; presence is shared by the
//! hubs) and a mesh RPC endpoint ([`RelayPeer`](crate::api::RelayPeer)); sessions to other nodes
//! carry both ([`attach_session_rpc`]).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use axum::response::IntoResponse;
use id_effect::DynEnv;
use id_effect_kit::axum::realtime::ws::WsConfig;
use id_effect_kit::axum::route::{self, AccessLayer, RouteAccess, RouteInfo};
use id_effect_kit::axum::server::h3::{Http3Options, serve_h3};
use id_effect_kit::axum::server::security::{ContentSecurityPolicy, CookieKey, SecurityLayer};
use id_effect_kit::axum::server::webtransport::{self as wt, WebTransportConfig};
use id_effect_kit::axum::server::{AltSvcLayer, ConnectionLimits, HostTls, ServerTls};
use id_effect_kit::axum::{
  BodyLimitLayer, HostConfig, ReadinessState, RequestIdLayer, ServerHandle, TimeoutLayer,
};
use id_effect_kit::node::transport::quic::{QuicConfig, QuicTransport};
use id_effect_kit::node::{
  BehaviorRegistry, Cookie, DeliveryConfig, Listener, Mesh, NodeId, Session, Transport,
  TransportError,
};
use id_effect_kit::platform::quic::{QuicEndpoint, QuicEndpointConfig};
use id_effect_kit::platform::tls::{Identity, NodeCertificate};
use id_effect_kit::posture::{Posture, Surface};
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
use id_effect_process::{NodeName, ProcessNode};
use tokio::sync::oneshot;

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

/// How many times [`RelayNode::start`] tries a fresh port when asked for port 0 and the TCP port
/// matching the UDP one is taken.
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
  /// The address (TCP and UDP, one port; port 0 picks a free one).
  pub bind: SocketAddr,
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
      .host(&self.host)
      // HTTP/3 is served by the node's own endpoint (shared with the mesh), not by the host:
      // on, with the host's certificate.
      .http3(true, self.host.tls.is_some())
      .http3_options(&self.h3)
      .quic_retry(self.retry)
      .tls_certificate(&self.certificate)
  }
}

/// A running node.
pub struct RelayNode {
  id: NodeId,
  addr: SocketAddr,
  state: Arc<NodeState>,
  rpc: MeshRpc,
  endpoint: QuicEndpoint,
  dialer: QuicTransport,
  cookie: Cookie,
  links: Arc<Mutex<Vec<SessionLink>>>,
  tcp: ServerHandle,
  stop_h3: Mutex<Option<oneshot::Sender<()>>>,
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
      .finish_non_exhaustive()
  }
}

impl RelayNode {
  /// Starts a node (inside a tokio runtime): the mesh, the channels hub and the mesh RPC
  /// endpoint, then the QUIC endpoint (HTTP/3, WebTransport, `idm/1`) and TLS on TCP on the same
  /// port. Peers are joined with [`connect`](Self::connect).
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
    let host = HostConfig {
      bind_host: config.bind.ip().to_string(),
      bind_port: config.bind.port(),
      // HTTP/3 runs on the node's own endpoint, shared with the mesh (D7).
      http3: false,
      ..HostConfig::default()
    }
    .with_tls(HostTls::provided(tls));
    let limits = ConnectionLimits::default();
    let quic_config =
      QuicEndpointConfig::new(identity.clone()).with_limits(wt::quic_limits(&limits));
    let h3 = Http3Options::default().webtransport();
    let surfaces = Surfaces {
      ws,
      wt: wt_config,
      h3: h3.clone(),
      host: host.clone(),
      retry: quic_config.retry(),
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
    let (endpoint, tcp) = bind(&config, &host, quic_config, &limits, app.clone()).await?;
    let addr = endpoint.local_addr();

    let (stop_h3, stopped) = oneshot::channel::<()>();
    let served = endpoint.clone();
    let h3_options = h3.clone();
    tokio::spawn(async move {
      let shutdown = async move {
        let _ = stopped.await;
      };
      if let Err(e) = serve_h3(&served, app, limits, h3_options, shutdown).await {
        eprintln!("relay: http/3 stopped: {e}");
      }
    });

    let quic = config.mesh.quic_config()?;
    let listener = QuicTransport::on_endpoint(config.mesh.quic_config()?, &endpoint)
      .listen("")
      .await
      .map_err(|e| start_error("mesh listener", e))?;
    let dialer = QuicTransport::on_endpoint(quic, &endpoint);
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
      state,
      rpc,
      endpoint,
      dialer,
      cookie: config.cookie,
      links,
      tcp,
      stop_h3: Mutex::new(Some(stop_h3)),
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

  /// The node's address: TCP and UDP on this one port.
  pub fn addr(&self) -> SocketAddr {
    self.addr
  }

  /// The node's state (channels, history readers), for operators and tests.
  pub fn state(&self) -> &Arc<NodeState> {
    &self.state
  }

  /// The node's mesh RPC.
  pub fn mesh(&self) -> &MeshRpc {
    &self.rpc
  }

  /// The node's QUIC endpoint (statistics: accepted connections, refused client certificates).
  pub fn endpoint(&self) -> &QuicEndpoint {
    &self.endpoint
  }

  /// Joins the node at `peer` (its [`addr`](Self::addr)): a QUIC connection on `idm/1` from
  /// this node's own port, mutual TLS, the cookie handshake, then one session carrying the
  /// channels and mesh RPC.
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
  /// allowlists, HTTP/3 with TLS on the shared endpoint, 0-RTT off, Retry on, the served
  /// certificate.
  pub fn posture(&self, base: Posture) -> Posture {
    self.surfaces.posture(base)
  }

  /// Stops the node: the rooms' members leave, the QUIC endpoint closes (peers see the node go
  /// down at once), HTTP/3 and TCP drain.
  pub async fn shutdown(&self) {
    self.state.channels().shutdown();
    self.endpoint.close();
    self
      .links
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();
    if let Some(stop) = self
      .stop_h3
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take()
    {
      let _ = stop.send(());
    }
    self.tcp.shutdown().await;
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

/// Binds the QUIC endpoint and TLS on TCP on one port: the configured one, or (port 0) a free
/// UDP port whose TCP twin is free too.
async fn bind(
  config: &NodeConfig,
  host: &HostConfig,
  quic: QuicEndpointConfig,
  limits: &ConnectionLimits,
  app: axum::Router,
) -> Result<(QuicEndpoint, ServerHandle), StartError> {
  let attempts = if config.bind.port() == 0 {
    BIND_ATTEMPTS
  } else {
    1
  };
  let mut last = String::new();
  for _ in 0..attempts {
    let endpoint =
      QuicEndpoint::bind(config.bind, quic.clone()).map_err(|e| start_error("quic", e))?;
    let port = endpoint.local_addr().port();
    let host = HostConfig {
      bind_port: port,
      ..host.clone()
    };
    let advertised = config.advertise_h3_port.unwrap_or(port);
    let tcp_app = app.clone().layer(AltSvcLayer::h3(advertised));
    match ServerHandle::start_with_limits(&host, tcp_app, ReadinessState::new(true), limits.clone())
      .await
    {
      Ok(tcp) => return Ok((endpoint, tcp)),
      Err(e) => {
        last = e.to_string();
        endpoint.close();
      }
    }
  }
  Err(StartError(format!("tcp: {last}")))
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
