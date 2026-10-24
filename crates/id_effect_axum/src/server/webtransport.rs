//! WebTransport sessions on the HTTP/3 adapter (feature `webtransport`, ADR 0014 "HTTP/3",
//! `h3-webtransport =0.1.2`).
//!
//! A WebTransport session starts with an extended `CONNECT` (`:protocol = webtransport`) on an
//! HTTP/3 connection. Here that CONNECT is a **typed route** ([`endpoint`]): it goes through the
//! router like any request, so the router's deny-by-default access layer, an `Authenticate`
//! layer and the route's own guards (`.public()` or `.guard(g)`, H8) decide it, and the
//! `Origin` header is checked against the same allowlist type as WebSocket upgrades
//! ([`OriginPolicy`], ADR 0013 R9, ADR 0014 C4: empty by default, which refuses every browser
//! origin). When the route accepts, the session takes its HTTP/3 connection over: the client's
//! bidirectional streams reach the handler ([`WebTransportSession::accept_bi`]), and plain HTTP/3
//! requests that still arrive on the connection are served by the router as before.
//!
//! Turn it on with
//! [`Http3Options::webtransport`](crate::server::h3::Http3Options::webtransport) on an endpoint
//! whose limits carry QUIC datagrams ([`quic_limits`]): HTTP/3 datagrams are negotiated per
//! connection and need the transport parameter. The session hook rides the endpoint's one `h3`
//! route: no route of its own is registered.
//!
//! **On the host.** The host entry points ([`serve_router`](crate::server::serve_router),
//! [`ServerHandle`](crate::server::ServerHandle), [`serve`](crate::server::serve)) do both for
//! their own HTTP/3 endpoint when
//! [`HostConfig::webtransport`](crate::server::HostConfig::webtransport) is on
//! (`server.http3.webtransport = true`; off by default): put the [`endpoint`] route in the app
//! they serve and give the host TLS certificate files; no `serve_h3` endpoint of the app's own
//! is needed. The route's `Origin` allowlist and session caps apply there as anywhere.
//!
//! ## Limits (ADR 0013 R4)
//!
//! | Limit | Default |
//! |---|---|
//! | Origin allowlist | empty: every browser origin refused (**403**) |
//! | Sessions per endpoint / per client IP | 10,000 / 16 (**429** before the session starts) |
//! | Sessions per connection | 1 (`SETTINGS_WEBTRANSPORT_MAX_SESSIONS`) |
//! | Streams per session | the endpoint's QUIC stream credits (100 bidirectional, [`quic_limits`]) |
//! | Streams queued for the handler | 16: a handler that does not accept stops the reads |
//! | Idle | the QUIC idle timeout (30 s); WebTransport has no ping of its own |
//!
//! On shutdown a session's connection is closed (`H3_NO_ERROR`): WebTransport has no graceful
//! drain, so clients reconnect (the RPC transport resumes its streams).
//!
//! [`client`] is a minimal WebTransport client over the platform QUIC client config, for Rust
//! peers and tests (browsers bring their own).
//!
//! ```rust,no_run
//! use axum::Router;
//! use id_effect::{DynEnv, succeed};
//! use id_effect_axum::route::Router as TypedRouter;
//! use id_effect_axum::server::ConnectionLimits;
//! use id_effect_axum::server::h3::{Http3Options, serve_h3};
//! use id_effect_axum::server::webtransport::{self, WebTransportConfig};
//! use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};
//! use id_effect_platform::tls::{Identity, NodeCertificate};
//!
//! # async fn run() -> std::io::Result<()> {
//! let config = WebTransportConfig::default()
//!   .allowed_origins(["https://app.example.com"])
//!   .unwrap();
//! let route = webtransport::endpoint("/wt", config, |_: &mut DynEnv, _caps, mut session| {
//!   id_effect::from_async(move || async move {
//!     while let Some(stream) = session.accept_bi().await {
//!       drop(stream);
//!     }
//!     Ok::<_, id_effect::Never>(())
//!   })
//! })
//! .public();
//! let app: Router = TypedRouter::new().route(route).with_state(DynEnv::new()).into_axum();
//! let limits = ConnectionLimits::default();
//! let identity = Identity::from_certificate(&NodeCertificate::self_signed("localhost").unwrap())
//!   .unwrap();
//! let endpoint = QuicEndpoint::bind(
//!   "127.0.0.1:8443".parse().unwrap(),
//!   QuicEndpointConfig::new(identity).with_limits(webtransport::quic_limits(&limits)),
//! )
//! .unwrap();
//! let options = Http3Options::default().webtransport();
//! serve_h3(&endpoint, app, limits, options, async {
//!   let _ = tokio::signal::ctrl_c().await;
//! })
//! .await
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use ::h3::ext::Protocol;
use ::h3::quic::BidiStream as _;
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodFilter;
use h3_webtransport::server::{AcceptedBi, WebTransportSession as H3Session};
use id_effect::{Effect, Never, from_async, succeed};
use id_effect_platform::quic::QuicLimits;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};
use tower_service::Service as _;

use super::connection::ConnectionLimits;
use super::h3::{Connection, FullStream, respond, serve_request};
use crate::client_ip::{ClientIp, TrustedProxies};
use crate::realtime::origin::{InvalidOrigin, OriginPolicy};
use crate::request_caps::RequestCaps;
use crate::route::{Endpoint, Problem, RouteInfo, Undecided};

/// Default open sessions per endpoint: 10,000.
pub const DEFAULT_MAX_SESSIONS: usize = 10_000;

/// Default open sessions per client IP and endpoint: 16.
pub const DEFAULT_MAX_SESSIONS_PER_IP: usize = 16;

/// Streams accepted from the client and queued for the handler.
pub const STREAM_BACKLOG: usize = 16;

/// `H3_NO_ERROR`: the session ended without loss.
const H3_NO_ERROR: u32 = 0x100;

const TARGET: &str = "id_effect_axum::server::webtransport";

/// The endpoint limits of an HTTP/3 endpoint serving WebTransport: [`super::h3::quic_limits`]
/// with QUIC datagrams on ([`QuicLimits::with_datagrams`], 64 KiB per connection).
pub fn quic_limits(limits: &ConnectionLimits) -> QuicLimits {
  super::h3::quic_limits(limits).with_datagrams()
}

/// Whether `request` is a WebTransport extended CONNECT.
pub fn is_connect<B>(request: &axum::http::Request<B>) -> bool {
  request.method() == Method::CONNECT
    && request.extensions().get::<Protocol>() == Some(&Protocol::WEB_TRANSPORT)
}

// --- the session cap --------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Slots {
  total: usize,
  per_ip: HashMap<IpAddr, usize>,
}

/// Bounds the open sessions of one endpoint, in total and per client IP. Cheap to clone (clones
/// share the counts).
#[derive(Clone, Debug)]
pub struct SessionCap {
  max_total: usize,
  max_per_ip: usize,
  slots: Arc<Mutex<Slots>>,
}

impl SessionCap {
  /// At most `max_total` sessions, `max_per_ip` from one client IP (each at least 1).
  pub fn new(max_total: usize, max_per_ip: usize) -> Self {
    Self {
      max_total: max_total.max(1),
      max_per_ip: max_per_ip.max(1),
      slots: Arc::default(),
    }
  }

  /// The total and per-IP limits.
  pub fn limits(&self) -> (usize, usize) {
    (self.max_total, self.max_per_ip)
  }

  /// The sessions open now.
  pub fn open(&self) -> usize {
    self.slots().total
  }

  fn slots(&self) -> std::sync::MutexGuard<'_, Slots> {
    self.slots.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// A slot for a session from `ip`, released when the permit drops.
  fn acquire(&self, ip: Option<IpAddr>) -> Option<SessionPermit> {
    let mut slots = self.slots();
    if slots.total >= self.max_total {
      return None;
    }
    if let Some(ip) = ip {
      let n = slots.per_ip.entry(ip).or_insert(0);
      if *n >= self.max_per_ip {
        return None;
      }
      *n += 1;
    }
    slots.total += 1;
    Some(SessionPermit {
      cap: self.clone(),
      ip,
    })
  }
}

/// One open session's slot.
#[derive(Debug)]
struct SessionPermit {
  cap: SessionCap,
  ip: Option<IpAddr>,
}

impl Drop for SessionPermit {
  fn drop(&mut self) {
    let mut slots = self.cap.slots();
    slots.total = slots.total.saturating_sub(1);
    if let Some(ip) = self.ip
      && let Some(n) = slots.per_ip.get_mut(&ip)
    {
      *n -= 1;
      if *n == 0 {
        slots.per_ip.remove(&ip);
      }
    }
  }
}

// --- configuration ----------------------------------------------------------------------------

/// Settings of a WebTransport [`endpoint`]; [`Default`] is the secure default.
#[derive(Clone, Debug)]
pub struct WebTransportConfig {
  origins: OriginPolicy,
  cap: SessionCap,
}

impl Default for WebTransportConfig {
  fn default() -> Self {
    Self {
      origins: OriginPolicy::deny_all(),
      cap: SessionCap::new(DEFAULT_MAX_SESSIONS, DEFAULT_MAX_SESSIONS_PER_IP),
    }
  }
}

impl WebTransportConfig {
  /// The secure defaults.
  pub fn new() -> Self {
    Self::default()
  }

  /// The `Origin` allowlist (default [`OriginPolicy::deny_all`]).
  #[must_use]
  pub fn origins(mut self, policy: OriginPolicy) -> Self {
    self.origins = policy;
    self
  }

  /// Allow exactly `origins` (see [`OriginPolicy::allow`]).
  ///
  /// # Errors
  ///
  /// [`InvalidOrigin`] for the first entry that is not an origin.
  pub fn allowed_origins<I, T>(self, origins: I) -> Result<Self, InvalidOrigin>
  where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
  {
    Ok(self.origins(OriginPolicy::allow(origins)?))
  }

  /// **Dangerous.** Admit every origin ([`OriginPolicy::dangerous_any_origin`], recorded).
  #[must_use]
  pub fn dangerous_any_origin(self) -> Self {
    self.origins(OriginPolicy::dangerous_any_origin())
  }

  /// At most `total` sessions on the endpoint and `per_ip` from one client IP (each at least 1).
  #[must_use]
  pub fn max_sessions(mut self, total: usize, per_ip: usize) -> Self {
    self.cap = SessionCap::new(total, per_ip);
    self
  }

  /// Share `cap` with other endpoints.
  #[must_use]
  pub fn session_cap(mut self, cap: SessionCap) -> Self {
    self.cap = cap;
    self
  }

  /// The `Origin` allowlist.
  pub fn origin_policy(&self) -> &OriginPolicy {
    &self.origins
  }

  /// The session cap.
  pub fn cap(&self) -> &SessionCap {
    &self.cap
  }
}

// --- sessions and streams ---------------------------------------------------------------------

/// The sending half of a WebTransport stream (`shutdown` finishes it).
pub type SendHalf = Box<dyn AsyncWrite + Send + Unpin>;

/// The receiving half of a WebTransport stream.
pub type RecvHalf = Box<dyn AsyncRead + Send + Unpin>;

/// One bidirectional WebTransport stream.
pub struct WebTransportStream {
  /// Bytes to the peer.
  pub send: SendHalf,
  /// Bytes from the peer.
  pub recv: RecvHalf,
}

impl fmt::Debug for WebTransportStream {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WebTransportStream").finish_non_exhaustive()
  }
}

/// What the connection hands the handler once the session is established.
pub(crate) struct SessionParts {
  streams: mpsc::Receiver<WebTransportStream>,
  conn: quinn::Connection,
}

/// One WebTransport session, as its handler sees it: the client's bidirectional streams, and
/// the connection to close.
pub struct WebTransportSession {
  streams: mpsc::Receiver<WebTransportStream>,
  conn: quinn::Connection,
  client_ip: Option<IpAddr>,
  _permit: SessionPermit,
}

impl fmt::Debug for WebTransportSession {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WebTransportSession")
      .field("remote", &self.conn.remote_address())
      .finish_non_exhaustive()
  }
}

impl WebTransportSession {
  /// The next bidirectional stream the client opened; `None` once the session is over.
  pub async fn accept_bi(&mut self) -> Option<WebTransportStream> {
    self.streams.recv().await
  }

  /// Ends the session: the connection closes with application error `code` and `reason`.
  pub fn close(&self, code: u32, reason: &str) {
    self.conn.close(code.into(), reason.as_bytes());
  }

  /// Resolves once the session's connection is closed.
  pub async fn closed(&self) {
    let _ = self.conn.closed().await;
  }

  /// The client's address ([`ClientIp`]: the peer, or the forwarded hop behind trusted
  /// proxies).
  pub fn client_ip(&self) -> Option<IpAddr> {
    self.client_ip
  }

  /// The QUIC peer address.
  pub fn remote_address(&self) -> SocketAddr {
    self.conn.remote_address()
  }
}

impl Drop for WebTransportSession {
  fn drop(&mut self) {
    // The handler is done with the session: so is the connection.
    self.conn.close(H3_NO_ERROR.into(), b"");
  }
}

/// The request extension through which a WebTransport route claims the session of its
/// CONNECT (the h3 adapter inserts it; one claim).
#[derive(Clone)]
struct Upgrade(Arc<Mutex<Option<oneshot::Receiver<SessionParts>>>>);

impl Upgrade {
  fn claim(&self) -> Option<oneshot::Receiver<SessionParts>> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
  }

  fn claimed(&self) -> bool {
    self
      .0
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_none()
  }
}

/// A CONNECT the route accepted, on its way to the connection task.
pub(crate) struct Handoff {
  connect: axum::http::Request<()>,
  stream: FullStream,
  parts: oneshot::Sender<SessionParts>,
}

/// The CONNECT's head again, with its `:protocol` (h3-webtransport checks it).
fn connect_head(head: &axum::http::Request<()>) -> axum::http::Request<()> {
  let mut connect = axum::http::Request::new(());
  *connect.method_mut() = head.method().clone();
  *connect.uri_mut() = head.uri().clone();
  *connect.version_mut() = head.version();
  *connect.headers_mut() = head.headers().clone();
  connect.extensions_mut().insert(Protocol::WEB_TRANSPORT);
  connect
}

/// Serves a WebTransport CONNECT through `app`: when its route accepted it (`200` and the
/// session claimed) the stream goes to the connection task, which answers it and starts the
/// session; otherwise the route's response is sent.
pub(crate) async fn serve_connect(
  head: axum::http::Request<()>,
  stream: FullStream,
  app: Router,
  peer: SocketAddr,
  handoffs: mpsc::Sender<Handoff>,
) {
  let (parts, claim) = oneshot::channel();
  let upgrade = Upgrade(Arc::new(Mutex::new(Some(claim))));
  let connect = connect_head(&head);
  let mut request = head.map(|()| Body::empty());
  request.extensions_mut().insert(ConnectInfo(peer));
  request.extensions_mut().insert(upgrade.clone());
  let mut app = app;
  let Ok(response) = app.call(request).await;
  if upgrade.claimed() && response.status() == StatusCode::OK {
    let handoff = Handoff {
      connect,
      stream,
      parts,
    };
    // The connection is gone otherwise: so is the stream.
    let _ = handoffs.send(handoff).await;
    return;
  }
  let (send, _recv) = stream.split();
  let _ = respond(send, response, false).await;
}

/// The session's side of the connection: accept the CONNECT, hand the session to its handler,
/// then accept streams (to the handler) and requests (to the router) until the handler, the
/// client or the server ends it.
pub(crate) async fn drive(
  h3: Connection,
  conn: quinn::Connection,
  handoff: Handoff,
  app: Router,
  peer: SocketAddr,
  signal: watch::Sender<()>,
) {
  let Handoff {
    connect,
    stream,
    parts,
  } = handoff;
  let session = match H3Session::accept(connect, stream, h3).await {
    Ok(session) => session,
    Err(error) => {
      tracing::debug!(target: TARGET, %error, "webtransport session refused");
      return;
    }
  };
  let (streams_tx, streams) = mpsc::channel(STREAM_BACKLOG);
  let handed = parts.send(SessionParts {
    streams,
    conn: conn.clone(),
  });
  if handed.is_err() {
    conn.close(H3_NO_ERROR.into(), b"");
    return;
  }
  loop {
    tokio::select! {
      accepted = session.accept_bi() => match accepted {
        Ok(Some(AcceptedBi::BidiStream(_, stream))) => {
          let (send, recv) = stream.split();
          let stream = WebTransportStream {
            send: Box::new(send),
            recv: Box::new(recv),
          };
          if streams_tx.send(stream).await.is_err() {
            break;
          }
        }
        Ok(Some(AcceptedBi::Request(head, stream))) => {
          tokio::spawn(serve_request(head, stream, app.clone(), peer));
        }
        Ok(None) | Err(_) => break,
      },
      uni = session.accept_uni() => match uni {
        // Unidirectional streams carry nothing here: dropped (the peer sees them stopped).
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => break,
      },
      () = streams_tx.closed() => break,
      () = signal.closed() => {
        conn.close(H3_NO_ERROR.into(), b"server shutting down");
        break;
      }
    }
  }
  drop(streams_tx);
  conn.close(H3_NO_ERROR.into(), b"");
}

// --- the route --------------------------------------------------------------------------------

fn refuse(status: StatusCode, title: &str, detail: &str) -> Response {
  Problem::new(status)
    .with_title(title)
    .with_detail(detail)
    .into_response()
}

/// Answers one CONNECT: WebTransport, Origin, session cap, the session claimed; then `200` and
/// the session fiber.
fn accept<S, E, F>(config: &WebTransportConfig, env: S, req: Request, f: F) -> Response
where
  S: Send + Sync + 'static,
  E: Send + 'static,
  F: FnOnce(&mut S, RequestCaps, WebTransportSession) -> Effect<(), E, S> + Send + 'static,
{
  if !is_connect(&req) {
    return refuse(
      StatusCode::BAD_REQUEST,
      "WebTransport CONNECT expected",
      "send an extended CONNECT with :protocol webtransport over HTTP/3",
    );
  }
  if let Err(rejection) = config.origins.check(req.headers()) {
    tracing::debug!(target: TARGET, %rejection, "webtransport session refused");
    return rejection.into_response();
  }
  // One claim per CONNECT (the h3 adapter's), so a second route never sees it.
  let Some(claim) = req.extensions().get::<Upgrade>().and_then(Upgrade::claim) else {
    return refuse(
      StatusCode::BAD_REQUEST,
      "WebTransport is not served here",
      "the HTTP/3 listener does not serve WebTransport (Http3Options::webtransport)",
    );
  };
  let none = TrustedProxies::none();
  let trusted = req.extensions().get::<TrustedProxies>().unwrap_or(&none);
  let client_ip = ClientIp::from_parts(req.extensions(), req.headers(), trusted).map(|c| c.0);
  let Some(permit) = config.cap.acquire(client_ip) else {
    let mut res = refuse(
      StatusCode::TOO_MANY_REQUESTS,
      "Too many sessions",
      "the endpoint has reached its WebTransport session limit",
    );
    res.headers_mut().insert(
      axum::http::header::RETRY_AFTER,
      axum::http::HeaderValue::from_static("1"),
    );
    return res;
  };
  let caps = req
    .extensions()
    .get::<RequestCaps>()
    .cloned()
    .unwrap_or_default();
  let fiber = from_async(move || async move { Ok::<_, Never>(claim.await) }).flat_map(
    move |parts| match parts {
      // The connection went away before the session started.
      Err(_) => succeed(()),
      Ok(SessionParts { streams, conn }) => {
        let session = WebTransportSession {
          streams,
          conn,
          client_ip,
          _permit: permit,
        };
        crate::executor::request_effect(env, move |s| f(s, caps, session))
          .exit::<Never>()
          .map(|_| ())
      }
    },
  );
  drop(crate::EffectExecutor::global().runtime().fork(fiber));
  StatusCode::OK.into_response()
}

/// A typed `CONNECT` route at `path` serving WebTransport sessions with `handler` (see the
/// [module docs](self)). Decide its access with `.public()` or `.guard(g)`; the guards (and an
/// `Authenticate` layer) run on the CONNECT, before the session starts.
///
/// Per session, `handler(&mut state, caps, session)` builds the effect that serves it, with the
/// CONNECT's [`RequestCaps`]. The session ends when the effect does (its connection closes),
/// when the client closes it, or on shutdown.
pub fn endpoint<S, E, F>(
  path: impl Into<String>,
  config: WebTransportConfig,
  handler: F,
) -> Endpoint<S, Undecided>
where
  S: Clone + Send + Sync + 'static,
  E: Send + 'static,
  F: Fn(&mut S, RequestCaps, WebTransportSession) -> Effect<(), E, S>
    + Clone
    + Send
    + Sync
    + 'static,
{
  let config = Arc::new(config);
  let mut info = RouteInfo::new(Method::CONNECT, path);
  info.handler = Some(std::any::type_name::<F>());
  let method_router = axum::routing::on(
    MethodFilter::CONNECT,
    move |State(env): State<S>, req: Request| {
      let config = Arc::clone(&config);
      let f = handler.clone();
      async move { accept(&config, env, req, f) }
    },
  );
  Endpoint::from_parts(info, method_router)
}

// --- the host entry points --------------------------------------------------------------------

/// The HTTP/3 side of a host entry point that serves WebTransport
/// ([`HostConfig::webtransport`](crate::HostConfig::webtransport), F43): its own endpoint on the
/// TCP port number, with QUIC datagrams ([`quic_limits`]), served with
/// [`Http3Options::webtransport`](super::h3::Http3Options::webtransport).
#[derive(Debug)]
pub(crate) struct HostWebTransport {
  endpoint: id_effect_platform::quic::QuicEndpoint,
}

impl HostWebTransport {
  /// Binds UDP on `tcp`'s address when `config` turns HTTP/3 on and `transport` is TLS with an
  /// HTTP/3 identity (certificate files); `None` otherwise, and the reason is logged.
  pub(crate) fn bind(
    config: &crate::HostConfig,
    transport: &super::connection::HostTransport,
    tcp: SocketAddr,
    limits: &ConnectionLimits,
  ) -> std::io::Result<Option<Self>> {
    if !config.http3 || !transport.is_tls() {
      tracing::warn!(
        target: "id_effect_axum::posture",
        "{}",
        crate::server::bootstrap::WEBTRANSPORT_WITHOUT_HTTP3_WARNING
      );
      return Ok(None);
    }
    let Some(identity) = transport.http3_identity() else {
      tracing::warn!(
        target: "id_effect_axum::posture",
        "WebTransport is on but the TLS settings were provided (HostTls::Provided), which \
         carry no certificate identity for QUIC; serving TCP only, without HTTP/3 or \
         WebTransport (use certificate files)"
      );
      return Ok(None);
    };
    let quic = id_effect_platform::quic::QuicEndpointConfig::new(identity.clone())
      .with_limits(quic_limits(limits));
    let endpoint = id_effect_platform::quic::QuicEndpoint::bind(tcp, quic)
      .map_err(|e| std::io::Error::other(format!("http3: {e}")))?;
    Ok(Some(Self { endpoint }))
  }

  /// The UDP port (the TCP one).
  pub(crate) fn port(&self) -> u16 {
    self.endpoint.local_addr().port()
  }

  /// [`serve_h3`](super::h3::serve_h3) with WebTransport on, then closes the endpoint.
  pub(crate) async fn serve<F>(
    self,
    app: Router,
    limits: ConnectionLimits,
    shutdown: F,
  ) -> std::io::Result<()>
  where
    F: std::future::Future<Output = ()> + Send + 'static,
  {
    let options = super::h3::Http3Options::default().webtransport();
    let served = super::h3::serve_h3(&self.endpoint, app, limits, options, shutdown).await;
    self.endpoint.close();
    served
  }
}

// --- a client ---------------------------------------------------------------------------------

/// A minimal WebTransport client (draft 02, as Chromium speaks it) over the platform QUIC
/// client config: one session per connection, bidirectional streams. For Rust peers and tests;
/// it sends the HTTP/3 SETTINGS a WebTransport server needs, the extended CONNECT, and the
/// stream headers, and leaves the rest to QUIC.
pub mod client {
  use std::net::SocketAddr;

  use bytes::{Buf, BufMut, Bytes, BytesMut};
  use id_effect_platform::quic::{ALPN_H3, QuicLimits, client_config};
  use tokio_rustls::rustls;

  use super::WebTransportStream;

  /// The rustls client config [`connect`] takes (for example from
  /// `id_effect_platform::tls::client_tls_config`).
  pub type TlsConfig = rustls::ClientConfig;

  /// Why a session did not start.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ConnectError {
    /// The QUIC connection or the HTTP/3 exchange failed.
    Transport(String),
    /// The server answered the CONNECT with this status (not 200).
    Refused(u16),
  }

  impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      match self {
        ConnectError::Transport(why) => write!(f, "webtransport: {why}"),
        ConnectError::Refused(status) => write!(f, "webtransport CONNECT refused: {status}"),
      }
    }
  }

  impl std::error::Error for ConnectError {}

  fn transport(e: impl std::fmt::Display) -> ConnectError {
    ConnectError::Transport(e.to_string())
  }

  /// What the CONNECT carries besides `:authority` and `:path`.
  #[derive(Clone, Debug, Default)]
  pub struct ConnectRequest {
    /// The `Origin` header, as a browser sends it.
    pub origin: Option<String>,
    /// Further headers (lower-case names).
    pub headers: Vec<(String, String)>,
  }

  /// One WebTransport session.
  pub struct WebTransportClient {
    conn: quinn::Connection,
    session_id: u64,
    // The control stream and the CONNECT stream live as long as the session.
    _control: quinn::SendStream,
    _connect: (quinn::SendStream, quinn::RecvStream),
    _endpoint: quinn::Endpoint,
  }

  impl std::fmt::Debug for WebTransportClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.debug_struct("WebTransportClient")
        .field("session_id", &self.session_id)
        .finish_non_exhaustive()
    }
  }

  // HTTP/3 and WebTransport (draft 02) identifiers.
  const STREAM_CONTROL: u64 = 0x00;
  const FRAME_HEADERS: u64 = 0x01;
  const FRAME_SETTINGS: u64 = 0x04;
  const SETTINGS_ENABLE_CONNECT_PROTOCOL: u64 = 0x08;
  const SETTINGS_H3_DATAGRAM: u64 = 0x33;
  const SETTINGS_ENABLE_WEBTRANSPORT: u64 = 0x2b60_3742;
  const WEBTRANSPORT_BIDI_STREAM: u64 = 0x41;

  /// Largest response header frame read (the server's CONNECT answer).
  const MAX_HEADERS: usize = 16 * 1024;

  /// Connects to `addr` (TLS server name `server_name`, verified by `tls`) and opens a session
  /// at `path` (with its query) with `request`'s headers.
  ///
  /// # Errors
  ///
  /// [`ConnectError::Refused`] when the server answers the CONNECT with another status than
  /// 200; [`ConnectError::Transport`] when QUIC or HTTP/3 fail.
  pub async fn connect(
    addr: SocketAddr,
    server_name: &str,
    tls: rustls::ClientConfig,
    path: &str,
    request: &ConnectRequest,
  ) -> Result<WebTransportClient, ConnectError> {
    let bind = if addr.is_ipv6() {
      SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
    } else {
      SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0))
    };
    let limits = QuicLimits::default().with_datagrams();
    let config = client_config(tls, &[ALPN_H3], &limits).map_err(transport)?;
    let mut endpoint = quinn::Endpoint::client(bind).map_err(transport)?;
    endpoint.set_default_client_config(config);
    let conn = endpoint
      .connect(addr, server_name)
      .map_err(transport)?
      .await
      .map_err(transport)?;
    // Our SETTINGS on the control stream (kept open: closing it is a connection error).
    let mut control = conn.open_uni().await.map_err(transport)?;
    let mut buf = BytesMut::new();
    put_varint(&mut buf, STREAM_CONTROL);
    let mut settings = BytesMut::new();
    for id in [
      SETTINGS_ENABLE_CONNECT_PROTOCOL,
      SETTINGS_H3_DATAGRAM,
      SETTINGS_ENABLE_WEBTRANSPORT,
    ] {
      put_varint(&mut settings, id);
      put_varint(&mut settings, 1);
    }
    put_frame(&mut buf, FRAME_SETTINGS, &settings);
    control.write_all(&buf).await.map_err(transport)?;
    // The server's SETTINGS first, as a browser waits for them; its other streams drained.
    let (settings_tx, settings_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(drain_uni(conn.clone(), settings_tx));
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), settings_rx).await;
    let (mut send, mut recv) = conn.open_bi().await.map_err(transport)?;
    let session_id = send.id().index() << 2;
    let authority = server_name_with_port(server_name, addr.port());
    let block = connect_block(&authority, path, request);
    let mut frame = BytesMut::new();
    put_frame(&mut frame, FRAME_HEADERS, &block);
    send.write_all(&frame).await.map_err(transport)?;
    let status = read_status(&mut recv).await?;
    if status != 200 {
      return Err(ConnectError::Refused(status));
    }
    Ok(WebTransportClient {
      conn,
      session_id,
      _control: control,
      _connect: (send, recv),
      _endpoint: endpoint,
    })
  }

  fn server_name_with_port(name: &str, port: u16) -> String {
    format!("{name}:{port}")
  }

  /// Reads the server's unidirectional streams (control, QPACK) and discards what they carry;
  /// the first data on the first of them (the control stream's SETTINGS) signals `settings`.
  async fn drain_uni(conn: quinn::Connection, settings: tokio::sync::oneshot::Sender<()>) {
    let mut settings = Some(settings);
    while let Ok(mut recv) = conn.accept_uni().await {
      let mut signal = settings.take();
      tokio::spawn(async move {
        let mut chunk = [0_u8; 1024];
        // Read what the server sends there for the life of the connection.
        while let Ok(Some(_)) = recv.read(&mut chunk).await {
          if let Some(signal) = signal.take() {
            let _ = signal.send(());
          }
        }
      });
    }
  }

  impl WebTransportClient {
    /// Opens a bidirectional stream of the session.
    ///
    /// # Errors
    ///
    /// When the connection is gone.
    pub async fn open_bi(&self) -> Result<WebTransportStream, ConnectError> {
      let (mut send, recv) = self.conn.open_bi().await.map_err(transport)?;
      let mut header = BytesMut::new();
      put_varint(&mut header, WEBTRANSPORT_BIDI_STREAM);
      put_varint(&mut header, self.session_id);
      send.write_all(&header).await.map_err(transport)?;
      Ok(WebTransportStream {
        send: Box::new(send),
        recv: Box::new(recv),
      })
    }

    /// Ends the session (closes its connection with `code`).
    pub fn close(&self, code: u32, reason: &str) {
      self.conn.close(code.into(), reason.as_bytes());
    }

    /// Resolves once the connection is closed, with the reason.
    pub async fn closed(&self) -> quinn::ConnectionError {
      self.conn.closed().await
    }

    /// The session id (the CONNECT stream's id).
    pub fn session_id(&self) -> u64 {
      self.session_id
    }

    /// The QUIC connection, for tests that need its statistics or a raw stream.
    pub fn connection(&self) -> &quinn::Connection {
      &self.conn
    }
  }

  // --- wire helpers -----------------------------------------------------------------------

  /// A QUIC variable-length integer (RFC 9000 §16).
  pub(crate) fn put_varint(buf: &mut BytesMut, v: u64) {
    if v < 1 << 6 {
      buf.put_u8(v as u8);
    } else if v < 1 << 14 {
      buf.put_u16(0x4000 | v as u16);
    } else if v < 1 << 30 {
      buf.put_u32(0x8000_0000 | v as u32);
    } else {
      buf.put_u64(0xc000_0000_0000_0000 | v);
    }
  }

  /// Reads a QUIC variable-length integer, `None` while incomplete.
  pub(crate) fn get_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1usize << (first >> 6);
    let bytes = buf.get(..len)?;
    let mut v = u64::from(first & 0x3f);
    for b in &bytes[1..] {
      v = (v << 8) | u64::from(*b);
    }
    Some((v, len))
  }

  fn put_frame(buf: &mut BytesMut, kind: u64, payload: &[u8]) {
    put_varint(buf, kind);
    put_varint(buf, payload.len() as u64);
    buf.put_slice(payload);
  }

  /// A QPACK prefixed integer (RFC 9204 §4.1.1) with `prefix` bits, `flags` in the high bits.
  pub(crate) fn put_prefix_int(buf: &mut BytesMut, prefix: u8, flags: u8, v: u64) {
    let max = (1u64 << prefix) - 1;
    if v < max {
      buf.put_u8(flags | v as u8);
      return;
    }
    buf.put_u8(flags | max as u8);
    let mut rest = v - max;
    while rest >= 128 {
      buf.put_u8((rest % 128) as u8 | 0x80);
      rest /= 128;
    }
    buf.put_u8(rest as u8);
  }

  /// Reads a QPACK prefixed integer; the rest of the input.
  pub(crate) fn get_prefix_int(buf: &[u8], prefix: u8) -> Option<(u64, &[u8])> {
    let (first, mut rest) = buf.split_first()?;
    let max = (1u64 << prefix) - 1;
    let mut v = u64::from(*first) & max;
    if v < max {
      return Some((v, rest));
    }
    let mut shift = 0;
    loop {
      let (b, tail) = rest.split_first()?;
      rest = tail;
      v += u64::from(b & 0x7f) << shift;
      shift += 7;
      if b & 0x80 == 0 || shift > 56 {
        return Some((v, rest));
      }
    }
  }

  fn put_string(buf: &mut BytesMut, prefix: u8, flags: u8, s: &str) {
    put_prefix_int(buf, prefix, flags, s.len() as u64);
    buf.put_slice(s.as_bytes());
  }

  /// A literal field line with a static name reference (`01NT` + 4-bit index).
  fn literal_static_name(buf: &mut BytesMut, index: u64, value: &str) {
    put_prefix_int(buf, 4, 0x50, index);
    put_string(buf, 7, 0, value);
  }

  /// A literal field line with a literal name (`001NH` + 3-bit length).
  fn literal_name(buf: &mut BytesMut, name: &str, value: &str) {
    put_prefix_int(buf, 3, 0x20, name.len() as u64);
    buf.put_slice(name.as_bytes());
    put_string(buf, 7, 0, value);
  }

  /// The CONNECT's QPACK field section: static references, no dynamic table, no Huffman.
  pub(crate) fn connect_block(authority: &str, path: &str, request: &ConnectRequest) -> BytesMut {
    let mut block = BytesMut::new();
    // Required insert count 0, base 0.
    block.put_u8(0);
    block.put_u8(0);
    // `:method CONNECT` (15) and `:scheme https` (23), indexed static.
    put_prefix_int(&mut block, 6, 0xc0, 15);
    put_prefix_int(&mut block, 6, 0xc0, 23);
    literal_static_name(&mut block, 0, authority);
    literal_static_name(&mut block, 1, path);
    literal_name(&mut block, ":protocol", "webtransport");
    if let Some(origin) = &request.origin {
      literal_static_name(&mut block, 90, origin);
    }
    for (name, value) in &request.headers {
      literal_name(&mut block, name, value);
    }
    block
  }

  /// The status of the static table's `:status` entries.
  fn static_status(index: u64) -> Option<u16> {
    const STATUSES: [(u64, u16); 14] = [
      (24, 103),
      (25, 200),
      (26, 304),
      (27, 404),
      (28, 503),
      (63, 100),
      (64, 204),
      (65, 206),
      (66, 302),
      (67, 400),
      (68, 403),
      (69, 421),
      (70, 425),
      (71, 500),
    ];
    STATUSES
      .iter()
      .find(|(i, _)| *i == index)
      .map(|(_, status)| *status)
  }

  /// Digits of a Huffman-coded (RFC 7541 Appendix B) status: `0`-`2` take five bits,
  /// `3`-`9` six.
  pub(crate) fn huffman_digits(bytes: &[u8]) -> Option<u16> {
    let mut bits = bytes
      .iter()
      .flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1));
    let mut take = |n: usize| -> Option<u8> {
      let mut v = 0;
      for _ in 0..n {
        v = (v << 1) | bits.next()?;
      }
      Some(v)
    };
    let mut status = 0u16;
    for _ in 0..3 {
      let five = take(5)?;
      let digit = if five <= 2 {
        five
      } else {
        let six = (five << 1) | take(1)?;
        if !(25..=31).contains(&six) {
          return None;
        }
        six - 22
      };
      status = status * 10 + u16::from(digit);
    }
    Some(status)
  }

  /// The `:status` of a response field section (the first field line, as servers send it).
  pub(crate) fn status_of(block: &[u8]) -> Option<u16> {
    let (_, rest) = get_prefix_int(block, 8)?;
    let (_, rest) = get_prefix_int(rest, 7)?;
    let first = *rest.first()?;
    if first & 0xc0 == 0xc0 {
      let (index, _) = get_prefix_int(rest, 6)?;
      return static_status(index);
    }
    if first & 0xf0 == 0x50 {
      let (index, rest) = get_prefix_int(rest, 4)?;
      static_status(index)?;
      let huffman = rest.first()? & 0x80 != 0;
      let (len, rest) = get_prefix_int(rest, 7)?;
      let value = rest.get(..usize::try_from(len).ok()?)?;
      return if huffman {
        huffman_digits(value)
      } else {
        std::str::from_utf8(value).ok()?.parse().ok()
      };
    }
    None
  }

  /// Reads frames of the CONNECT stream until the response HEADERS; its status.
  async fn read_status(recv: &mut quinn::RecvStream) -> Result<u16, ConnectError> {
    let mut buf = BytesMut::new();
    let mut chunk = [0_u8; 2048];
    loop {
      if let Some((kind, a)) = get_varint(&buf)
        && let Some((len, b)) = get_varint(&buf[a..])
      {
        let len = usize::try_from(len).map_err(transport)?;
        if len > MAX_HEADERS {
          return Err(transport("response headers too large"));
        }
        if buf.len() >= a + b + len {
          let payload: Bytes = Bytes::copy_from_slice(&buf[a + b..a + b + len]);
          buf.advance(a + b + len);
          if kind == FRAME_HEADERS {
            return status_of(&payload).ok_or_else(|| transport("unreadable response status"));
          }
          continue;
        }
      }
      match recv.read(&mut chunk).await.map_err(transport)? {
        Some(n) => buf.extend_from_slice(&chunk[..n]),
        None => return Err(transport("the CONNECT stream ended before a response")),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::client::*;
  use super::*;
  use bytes::BytesMut;

  #[test]
  fn varints_and_prefix_ints_round_trip() {
    for v in [
      0,
      63,
      64,
      16_383,
      16_384,
      (1 << 30) - 1,
      1 << 30,
      0x2b60_3742,
    ] {
      let mut buf = BytesMut::new();
      put_varint(&mut buf, v);
      assert_eq!(get_varint(&buf), Some((v, buf.len())), "{v}");
    }
    assert_eq!(get_varint(&[]), None);
    assert_eq!(get_varint(&[0x40]), None);
    for (prefix, v) in [
      (6, 15),
      (6, 62),
      (6, 63),
      (6, 90),
      (4, 14),
      (4, 90),
      (7, 300),
    ] {
      let mut buf = BytesMut::new();
      put_prefix_int(&mut buf, prefix, 0, v);
      let (got, rest) = get_prefix_int(&buf, prefix).unwrap();
      assert_eq!((got, rest.len()), (v, 0), "{prefix} {v}");
    }
    assert_eq!(get_prefix_int(&[0x3f], 6), None);
    assert_eq!(get_prefix_int(&[], 6), None);
  }

  #[test]
  fn statuses_decode_from_static_literal_and_huffman_lines() {
    // Indexed static: 200 (25), 403 (68).
    assert_eq!(status_of(&[0, 0, 0xc0 | 25]), Some(200));
    assert_eq!(status_of(&[0, 0, 0xff, 68 - 63]), Some(403));
    // Literal with a static name and a plain value.
    let mut block = BytesMut::from(&[0u8, 0][..]);
    put_prefix_int(&mut block, 4, 0x50, 24);
    put_prefix_int(&mut block, 7, 0, 3);
    block.extend_from_slice(b"429");
    assert_eq!(status_of(&block), Some(429));
    // Huffman "429": 4 = 011010, 2 = 00010, 9 = 011111, then EOS padding.
    // 011010 00010 011111 + 1111111 (padding to 24 bits) = 0x68 0x9f 0xff (with padding).
    let bits = "011010000100111111111111";
    let bytes: Vec<u8> = (0..3)
      .map(|i| u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap())
      .collect();
    assert_eq!(huffman_digits(&bytes), Some(429));
    let mut block = BytesMut::from(&[0u8, 0][..]);
    put_prefix_int(&mut block, 4, 0x50, 24);
    put_prefix_int(&mut block, 7, 0x80, 3);
    block.extend_from_slice(&bytes);
    assert_eq!(status_of(&block), Some(429));
    assert_eq!(huffman_digits(&[0xff]), None);
    assert_eq!(huffman_digits(&[0b0110_0000, 0]), None);
    // Unknown shapes.
    assert_eq!(status_of(&[0, 0, 0xc0 | 1]), None);
    assert_eq!(status_of(&[0, 0, 0x20]), None);
    assert_eq!(status_of(&[0, 0]), None);
    let mut block = BytesMut::from(&[0u8, 0][..]);
    put_prefix_int(&mut block, 4, 0x50, 24);
    put_prefix_int(&mut block, 7, 0, 3);
    block.extend_from_slice(b"x2");
    assert_eq!(status_of(&block), None);
  }

  #[test]
  fn connect_block_names_the_session() {
    let request = ConnectRequest {
      origin: Some("https://app.example".into()),
      headers: vec![("authorization".into(), "Bearer t".into())],
    };
    let block = connect_block("localhost:4433", "/rpc/wt?codec=x", &request);
    assert_eq!(&block[..4], &[0, 0, 0xc0 | 15, 0xc0 | 23]);
    let text = String::from_utf8_lossy(&block);
    for part in [
      "localhost:4433",
      "/rpc/wt?codec=x",
      ":protocol",
      "webtransport",
      "https://app.example",
      "authorization",
    ] {
      assert!(text.contains(part), "{part}");
    }
  }

  #[test]
  fn config_and_cap_defaults_are_bounded() {
    let config = WebTransportConfig::new();
    assert!(!config.origin_policy().is_any());
    assert_eq!(
      config.cap().limits(),
      (DEFAULT_MAX_SESSIONS, DEFAULT_MAX_SESSIONS_PER_IP)
    );
    let config = config.max_sessions(0, 0);
    assert_eq!(config.cap().limits(), (1, 1));
    assert!(WebTransportConfig::new().allowed_origins(["nope"]).is_err());
    let cap = SessionCap::new(2, 1);
    let config = WebTransportConfig::new().session_cap(cap.clone());
    let a: IpAddr = "192.0.2.1".parse().unwrap();
    let first = config.cap().acquire(Some(a)).unwrap();
    assert!(cap.acquire(Some(a)).is_none());
    let second = cap.acquire(None).unwrap();
    assert!(cap.acquire(None).is_none());
    assert_eq!(cap.open(), 2);
    drop((first, second));
    assert_eq!(cap.open(), 0);
    assert!(cap.acquire(Some(a)).is_some());
    let any = WebTransportConfig::new().dangerous_any_origin();
    assert!(any.origin_policy().is_any());
    assert_eq!(
      quic_limits(&ConnectionLimits::default()).datagram_receive_buffer,
      Some(id_effect_platform::quic::DEFAULT_DATAGRAM_RECEIVE_BUFFER)
    );
    let error = ConnectError::Refused(403);
    assert!(error.to_string().contains("403"));
    assert!(
      ConnectError::Transport("x".into())
        .to_string()
        .contains("x")
    );
  }

  mod live {
    use std::time::Duration;

    use id_effect::{DynEnv, Never, from_async};
    use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};
    use id_effect_platform::tls::{Identity, NodeCertificate, client_tls_config};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tower::ServiceExt as _;

    use super::super::client::{self, ConnectError, ConnectRequest};
    use super::super::*;
    use crate::route::Router as TypedRouter;
    use crate::server::h3::{Http3Options, serve_h3};

    const WAIT: Duration = Duration::from_secs(20);

    /// An echo route: every stream's bytes come back, then the stream ends.
    fn app(config: WebTransportConfig) -> Router {
      let route = endpoint(
        "/wt",
        config,
        |_: &mut DynEnv, _caps, mut session: WebTransportSession| {
          from_async(move || async move {
            assert!(session.client_ip().is_some());
            assert!(format!("{session:?}").contains("remote"));
            let _ = session.remote_address();
            while let Some(stream) = session.accept_bi().await {
              assert!(format!("{stream:?}").contains("WebTransportStream"));
              let WebTransportStream { mut send, mut recv } = stream;
              tokio::spawn(async move {
                let mut buf = Vec::new();
                let _ = recv.read_to_end(&mut buf).await;
                if buf == b"close" {
                  return;
                }
                let _ = send.write_all(&buf).await;
                let _ = send.shutdown().await;
              });
            }
            // The client is gone: so is the connection.
            session.closed().await;
            Ok::<_, Never>(())
          })
        },
      )
      .public();
      TypedRouter::new()
        .route(route)
        .with_state(DynEnv::new())
        .into_axum()
    }

    struct Served {
      addr: std::net::SocketAddr,
      tls: tokio_rustls::rustls::ClientConfig,
      stop: tokio::sync::oneshot::Sender<()>,
      done: tokio::task::JoinHandle<std::io::Result<()>>,
    }

    fn serve(config: WebTransportConfig) -> Served {
      let cert = NodeCertificate::self_signed("localhost").unwrap();
      let pin = cert.cert_sha256();
      let identity = Identity::from_certificate(&cert).unwrap();
      let limits = ConnectionLimits::default();
      let quic = QuicEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        QuicEndpointConfig::new(identity.clone()).with_limits(quic_limits(&limits)),
      )
      .unwrap();
      let addr = quic.local_addr();
      let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
      let app = app(config);
      let options = Http3Options::default().webtransport();
      assert!(options.webtransport_enabled());
      let done = tokio::spawn(async move {
        serve_h3(&quic, app, limits, options, async {
          let _ = stopped.await;
        })
        .await
      });
      let tls = client_tls_config(&identity, None, &[pin]).unwrap();
      Served {
        addr,
        tls,
        stop,
        done,
      }
    }

    fn from(origin: &str) -> ConnectRequest {
      ConnectRequest {
        origin: Some(origin.to_owned()),
        headers: vec![("x-test".into(), "1".into())],
      }
    }

    async fn echo(session: &client::WebTransportClient, bytes: &[u8]) -> Vec<u8> {
      let mut stream = session.open_bi().await.unwrap();
      stream.send.write_all(bytes).await.unwrap();
      stream.send.shutdown().await.unwrap();
      let mut out = Vec::new();
      tokio::time::timeout(WAIT, stream.recv.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
      out
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sessions_carry_streams_within_origin_and_session_limits() {
      let config = WebTransportConfig::default()
        .allowed_origins(["https://app.example"])
        .unwrap()
        .max_sessions(8, 1);
      let cap = config.cap().clone();
      let served = serve(config);
      let connect = |path: &'static str, request: ConnectRequest| {
        let tls = served.tls.clone();
        let addr = served.addr;
        async move { client::connect(addr, "localhost", tls, path, &request).await }
      };
      let session = connect("/wt", from("https://app.example")).await.unwrap();
      assert!(format!("{session:?}").contains("session_id"));
      assert_eq!(session.session_id() % 4, 0);
      let _ = session.connection().remote_address();
      // Two streams, one after the other and at once.
      assert_eq!(echo(&session, b"one").await, b"one");
      let (a, b) = tokio::join!(echo(&session, b"two"), echo(&session, b"three"));
      assert_eq!((a.as_slice(), b.as_slice()), (&b"two"[..], &b"three"[..]));
      // C4: a foreign origin is refused before the session starts.
      let refused = connect("/wt", from("https://evil.example")).await;
      assert_eq!(refused.unwrap_err(), ConnectError::Refused(403));
      // R4: one session per client IP here.
      let refused = connect("/wt", from("https://app.example")).await;
      assert_eq!(refused.unwrap_err(), ConnectError::Refused(429));
      // No route at that path.
      let refused = connect("/nope", from("https://app.example")).await;
      assert_eq!(refused.unwrap_err(), ConnectError::Refused(404));
      // The client closes: the handler sees the end, its slot frees.
      session.close(0, "done");
      let _ = session.closed().await;
      tokio::time::timeout(WAIT, async {
        while cap.open() > 0 {
          tokio::time::sleep(Duration::from_millis(20)).await;
        }
      })
      .await
      .unwrap();
      let again = connect("/wt", ConnectRequest::default()).await.unwrap();
      assert_eq!(echo(&again, b"four").await, b"four");
      // Shutdown closes the session's connection and the server returns.
      served.stop.send(()).unwrap();
      let closed = tokio::time::timeout(WAIT, again.closed()).await.unwrap();
      assert!(!closed.to_string().is_empty());
      tokio::time::timeout(WAIT, served.done)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stream_dropped_unanswered_ends_empty_and_shutdown_closes_the_session() {
      let served = serve(WebTransportConfig::default());
      let session = client::connect(
        served.addr,
        "localhost",
        served.tls.clone(),
        "/wt",
        &ConnectRequest::default(),
      )
      .await
      .unwrap();
      // The echo task drops the stream unanswered: the client reads an empty end.
      assert_eq!(echo(&session, b"close").await, b"");
      drop(served.stop);
      tokio::time::timeout(WAIT, session.closed()).await.unwrap();
    }

    #[tokio::test]
    async fn the_route_refuses_what_is_not_a_served_webtransport_connect() {
      let app = app(WebTransportConfig::default().dangerous_any_origin());
      // A CONNECT without `:protocol`.
      let req = axum::http::Request::builder()
        .method(Method::CONNECT)
        .uri("/wt")
        .body(Body::empty())
        .unwrap();
      let res = app.clone().oneshot(req).await.unwrap();
      assert_eq!(res.status(), StatusCode::BAD_REQUEST);
      // A WebTransport CONNECT that no HTTP/3 listener hands over (TCP, or WebTransport off).
      let mut req = axum::http::Request::builder()
        .method(Method::CONNECT)
        .uri("/wt")
        .body(Body::empty())
        .unwrap();
      req.extensions_mut().insert(Protocol::WEB_TRANSPORT);
      let res = app.oneshot(req).await.unwrap();
      assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
  }

  #[cfg(feature = "ws")]
  #[tokio::test]
  async fn websocket_sockets_ride_channels() {
    use crate::realtime::ws::{WsMessage, WsSocket};
    let (in_tx, in_rx) = mpsc::channel(1);
    let (out_tx, mut out_rx) = mpsc::channel(1);
    let socket = WsSocket::from_channels(in_rx, out_tx, Some("p".into()), None);
    assert_eq!((socket.protocol(), socket.client_ip()), (Some("p"), None));
    let sender = socket.sender();
    in_tx.send(WsMessage::from("in")).await.unwrap();
    let exit = crate::EffectExecutor::global()
      .run_effect(sender.send::<id_effect::HNil>("out"))
      .await;
    assert!(matches!(exit, id_effect::Exit::Success(())));
    assert_eq!(out_rx.recv().await, Some(WsMessage::from("out")));
    drop(out_rx);
    assert!(sender.is_closed());
  }

  #[test]
  fn connect_requests_are_recognised() {
    let mut req = axum::http::Request::new(());
    *req.method_mut() = Method::CONNECT;
    assert!(!is_connect(&req));
    req.extensions_mut().insert(Protocol::WEB_TRANSPORT);
    assert!(is_connect(&req));
    let head = connect_head(&req);
    assert!(is_connect(&head));
  }
}
