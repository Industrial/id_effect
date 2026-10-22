//! Effect-native WebSocket endpoints (feature `ws`; ADR 0014 E12.2, web.realtime #1 #6 #8).
//!
//! [`endpoint`] makes a typed route serving WebSocket upgrades. It has no access decision yet:
//! call `.public()` or `.guard(g)` before [`Router::route`](crate::route::Router::route) (H8).
//! Each accepted socket runs as its own supervised fiber:
//!
//! - the handler gets a [`WsSocket`]: a pull-based [`Stream`] of inbound [`WsMessage`]s and a
//!   [`WsSender`] (also a [`Sink`]) for outbound ones, both bounded;
//! - the handler runs as a child fiber of the socket's fiber, beside the connection driver that
//!   owns the wire (frames, pings, timers, limits);
//! - when the client goes away (or a limit or timer closes the socket) the handler is
//!   **interrupted** and its finalizers run; when the handler returns, its queued messages are
//!   flushed and the socket closes with 1000 (1011 when it failed).
//!
//! Secure defaults (see [`WsConfig`]): an `Origin` allowlist that refuses every browser origin
//! until configured ([`super::origin`]); frames and messages of at most 64 KiB (close 1009);
//! 100 inbound messages per second per socket (close 1008); control frames (WebSocket pings and
//! pongs, and the messages a [`WsConfig::classify`] hook calls [`WsFrameClass::Control`], such as
//! a protocol's flow-control credits) leave that rate and have their own cap, 4× the message rate
//! (close 1008 too); 16 sockets per client IP and
//! 10 000 per endpoint (**429** before the upgrade); a ping every 20 s, two missed pongs close
//! the socket (1001); 300 s idle timeout (1001). The handshake itself is refused with Problem
//! Details: **426** (not an upgrade, wrong version), **400** (bad key), **403** (origin).
//!
//! ```rust
//! use id_effect::{Effect, Never};
//! use id_effect_axum::RequestCaps;
//! use id_effect_axum::realtime::ws::{self, WsConfig, WsSocket};
//! use id_effect_axum::route::Router;
//!
//! /// Echoes every message back.
//! fn echo(_: &mut (), _caps: RequestCaps, socket: WsSocket) -> Effect<(), Never, ()> {
//!   let (incoming, out) = socket.into_parts::<Never, ()>();
//!   incoming
//!     .run_for_each_effect(move |msg| out.send(msg).catch(|_closed| id_effect::succeed(())))
//! }
//!
//! let config = WsConfig::default()
//!   .allowed_origins(["https://app.example.com"])
//!   .unwrap();
//! let app: axum::Router = Router::new()
//!   .route(ws::endpoint("/echo", config, echo).public())
//!   .with_state(())
//!   .into_axum();
//! # let _ = app;
//! ```

use std::fmt;
use std::future::{Future, poll_fn};
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use futures_util::{Sink as _, SinkExt as _, StreamExt as _};
use hyper_util::rt::TokioIo;
use id_effect::{
  Ctx, Effect, Executor, Exit, HNil, Never, Sink, Stream, from_async, succeed, suspend, sync,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tungstenite::Message;
use tungstenite::error::{Error as WireError, ProtocolError};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};

use super::ConnectionCap;
use super::origin::{InvalidOrigin, OriginPolicy};
use crate::client_ip::{ClientIp, TrustedProxies};
use crate::request_caps::RequestCaps;
use crate::route::{Endpoint, Problem, RouteInfo, Undecided};

/// Default largest frame and message: 64 KiB (ADR 0013 R4).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Default ping interval: 20 s.
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(20);
/// Default missed pongs before the socket closes: 2.
pub const DEFAULT_MISSED_PONGS: u32 = 2;
/// Default idle timeout (no message either way): 300 s.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Default inbound message rate per socket: 100 per second (burst 100).
pub const DEFAULT_MESSAGES_PER_SECOND: u32 = 100;
/// Default inbound control frame rate per socket, as a multiple of the message rate: 4 (ADR 0013
/// R4; [`WsConfig::control_messages_per_second`]).
pub const DEFAULT_CONTROL_RATE_FACTOR: u32 = 4;
/// Default sockets per client IP: 16.
pub const DEFAULT_MAX_SOCKETS_PER_IP: usize = super::DEFAULT_MAX_CONNECTIONS_PER_IP;
/// Default sockets per endpoint: 10 000.
pub const DEFAULT_MAX_SOCKETS: usize = super::DEFAULT_MAX_CONNECTIONS;

const SEC_WEBSOCKET_KEY: HeaderName = HeaderName::from_static("sec-websocket-key");
const SEC_WEBSOCKET_ACCEPT: HeaderName = HeaderName::from_static("sec-websocket-accept");
const SEC_WEBSOCKET_VERSION: HeaderName = HeaderName::from_static("sec-websocket-version");
const SEC_WEBSOCKET_PROTOCOL: HeaderName = HeaderName::from_static("sec-websocket-protocol");

/// Limits and policies of a WebSocket endpoint; [`Default`] is the secure default (see the
/// [module docs](self)).
#[derive(Clone, Debug)]
pub struct WsConfig {
  origins: OriginPolicy,
  max_frame_bytes: usize,
  max_message_bytes: usize,
  messages_per_second: u32,
  /// `None`: [`DEFAULT_CONTROL_RATE_FACTOR`] × `messages_per_second`.
  control_per_second: Option<u32>,
  classifier: Option<Classifier>,
  ping_interval: Duration,
  missed_pongs: u32,
  idle_timeout: Duration,
  close_timeout: Duration,
  max_sockets: usize,
  max_sockets_per_ip: usize,
  cap: Option<ConnectionCap>,
  inbox: usize,
  outbox: usize,
  protocols: Vec<String>,
}

impl Default for WsConfig {
  fn default() -> Self {
    Self {
      origins: OriginPolicy::deny_all(),
      max_frame_bytes: DEFAULT_MAX_MESSAGE_BYTES,
      max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
      messages_per_second: DEFAULT_MESSAGES_PER_SECOND,
      control_per_second: None,
      classifier: None,
      ping_interval: DEFAULT_PING_INTERVAL,
      missed_pongs: DEFAULT_MISSED_PONGS,
      idle_timeout: DEFAULT_IDLE_TIMEOUT,
      close_timeout: Duration::from_secs(5),
      max_sockets: DEFAULT_MAX_SOCKETS,
      max_sockets_per_ip: DEFAULT_MAX_SOCKETS_PER_IP,
      cap: None,
      inbox: 16,
      outbox: 16,
      protocols: Vec::new(),
    }
  }
}

impl WsConfig {
  /// The secure defaults.
  pub fn new() -> Self {
    Self::default()
  }

  /// The `Origin` allowlist (default: refuse every browser origin).
  #[must_use]
  pub fn origins(mut self, policy: OriginPolicy) -> Self {
    self.origins = policy;
    self
  }

  /// Allow exactly `origins` (see [`OriginPolicy::allow`]); the development profile passes
  /// [`DEV_ORIGINS`](super::origin::DEV_ORIGINS) here by configuration.
  ///
  /// # Errors
  ///
  /// [`InvalidOrigin`] for an entry that is not an origin.
  pub fn allowed_origins<I, T>(mut self, origins: I) -> Result<Self, InvalidOrigin>
  where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
  {
    self.origins = OriginPolicy::allow(origins)?;
    Ok(self)
  }

  /// Accept upgrades from every origin (see [`OriginPolicy::dangerous_any_origin`]; recorded).
  #[must_use]
  pub fn dangerous_any_origin(mut self) -> Self {
    self.origins = OriginPolicy::dangerous_any_origin();
    self
  }

  /// The largest frame and the largest (reassembled) message, in bytes (each at least 125);
  /// a larger one closes the socket with 1009.
  #[must_use]
  pub fn max_message_bytes(mut self, frame: usize, message: usize) -> Self {
    self.max_frame_bytes = frame.max(125);
    self.max_message_bytes = message.max(125);
    self
  }

  /// At most `n` (at least 1) inbound messages per second per socket, bursts of up to `n`;
  /// above it the socket closes with 1008.
  #[must_use]
  pub fn messages_per_second(mut self, n: u32) -> Self {
    self.messages_per_second = n.max(1);
    self
  }

  /// At most `n` (at least 1) inbound control frames per second per socket, bursts of up to `n`;
  /// above it the socket closes with 1008. Control frames are WebSocket pings and pongs and the
  /// messages the [`classify`](Self::classify) hook calls [`WsFrameClass::Control`]; they do not
  /// count against [`messages_per_second`](Self::messages_per_second). Default:
  /// [`DEFAULT_CONTROL_RATE_FACTOR`] × the message rate (it follows later changes of the message
  /// rate until this is set).
  #[must_use]
  pub fn control_messages_per_second(mut self, n: u32) -> Self {
    self.control_per_second = Some(n.max(1));
    self
  }

  /// Classify inbound data messages for the per-socket limiter: a message `classify` calls
  /// [`WsFrameClass::Control`] counts against the control cap
  /// ([`control_messages_per_second`](Self::control_messages_per_second)) instead of the message
  /// rate, so a protocol's own flow-control frames (credits, application pings) do not throttle
  /// its data. Default: every message is [`WsFrameClass::Data`].
  ///
  /// The hook runs on the connection driver for every inbound message, before it is queued: keep
  /// it cheap (look at small messages only) and conservative (call a message control only when
  /// it is a well-formed control frame). Classified messages still reach the handler unchanged.
  #[must_use]
  pub fn classify<F>(mut self, classify: F) -> Self
  where
    F: Fn(&WsMessage) -> WsFrameClass + Send + Sync + 'static,
  {
    self.classifier = Some(Classifier(Arc::new(classify)));
    self
  }

  /// Ping every `interval` (at least 1 ms); `missed` (at least 1) unanswered pings in a row
  /// close the socket with 1001.
  #[must_use]
  pub fn heartbeat(mut self, interval: Duration, missed: u32) -> Self {
    self.ping_interval = interval.max(Duration::from_millis(1));
    self.missed_pongs = missed.max(1);
    self
  }

  /// Close with 1001 after `timeout` (at least 1 ms) without a message either way.
  #[must_use]
  pub fn idle_timeout(mut self, timeout: Duration) -> Self {
    self.idle_timeout = timeout.max(Duration::from_millis(1));
    self
  }

  /// How long a control write (ping, close) or the final flush may take before the connection
  /// is dropped (default 5 s).
  #[must_use]
  pub fn close_timeout(mut self, timeout: Duration) -> Self {
    self.close_timeout = timeout.max(Duration::from_millis(1));
    self
  }

  /// At most `total` sockets on the endpoint and `per_ip` from one client IP (each at least 1).
  #[must_use]
  pub fn max_sockets(mut self, total: usize, per_ip: usize) -> Self {
    self.max_sockets = total.max(1);
    self.max_sockets_per_ip = per_ip.max(1);
    self.cap = None;
    self
  }

  /// Count sockets against `cap`, shared with other endpoints (replaces [`Self::max_sockets`]).
  #[must_use]
  pub fn connection_cap(mut self, cap: ConnectionCap) -> Self {
    self.cap = Some(cap);
    self
  }

  /// Inbound and outbound queue lengths, in messages (each at least 1): a full inbox stops
  /// reading the connection, a full outbox suspends [`WsSender::send`].
  #[must_use]
  pub fn buffers(mut self, inbox: usize, outbox: usize) -> Self {
    self.inbox = inbox.max(1);
    self.outbox = outbox.max(1);
    self
  }

  /// Subprotocols the endpoint speaks, in preference order: the first one the client offers
  /// is selected ([`WsSocket::protocol`]).
  #[must_use]
  pub fn protocols<I, T>(mut self, protocols: I) -> Self
  where
    I: IntoIterator<Item = T>,
    T: Into<String>,
  {
    self.protocols = protocols.into_iter().map(Into::into).collect();
    self
  }

  /// The `Origin` allowlist.
  pub fn origin_policy(&self) -> &OriginPolicy {
    &self.origins
  }

  /// The largest frame, in bytes.
  pub fn max_frame(&self) -> usize {
    self.max_frame_bytes
  }

  /// The largest message, in bytes.
  pub fn max_message(&self) -> usize {
    self.max_message_bytes
  }

  /// Inbound messages per second per socket.
  pub fn message_rate(&self) -> u32 {
    self.messages_per_second
  }

  /// Inbound control frames per second per socket.
  pub fn control_message_rate(&self) -> u32 {
    self.control_per_second.unwrap_or_else(|| {
      self
        .messages_per_second
        .saturating_mul(DEFAULT_CONTROL_RATE_FACTOR)
    })
  }

  /// How the limiter counts `msg` (see [`classify`](Self::classify)).
  pub fn frame_class(&self, msg: &WsMessage) -> WsFrameClass {
    self
      .classifier
      .as_ref()
      .map_or(WsFrameClass::Data, |classify| (classify.0)(msg))
  }

  /// The ping interval and the missed pongs that close the socket.
  pub fn heartbeat_settings(&self) -> (Duration, u32) {
    (self.ping_interval, self.missed_pongs)
  }

  /// The idle timeout.
  pub fn idle(&self) -> Duration {
    self.idle_timeout
  }

  /// The socket limits `(total, per_ip)`.
  pub fn socket_limits(&self) -> (usize, usize) {
    match &self.cap {
      Some(cap) => (cap.max_total(), cap.max_per_ip()),
      None => (self.max_sockets, self.max_sockets_per_ip),
    }
  }

  fn wire(&self) -> WebSocketConfig {
    WebSocketConfig::default()
      .max_frame_size(Some(self.max_frame_bytes))
      .max_message_size(Some(self.max_message_bytes))
      // Bound what buffers on a slow client: a few messages, then `send` waits.
      .max_write_buffer_size(self.max_message_bytes.saturating_mul(4).max(256 * 1024))
  }

  fn cap(&self) -> ConnectionCap {
    self
      .cap
      .clone()
      .unwrap_or_else(|| ConnectionCap::new(self.max_sockets, self.max_sockets_per_ip))
  }
}

/// How the per-socket limiter counts an inbound message ([`WsConfig::classify`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WsFrameClass {
  /// Application data: counted against [`WsConfig::messages_per_second`].
  Data,
  /// Protocol control (flow-control credits, application pings): counted against
  /// [`WsConfig::control_messages_per_second`].
  Control,
}

/// A [`WsConfig::classify`] hook.
#[derive(Clone)]
struct Classifier(Arc<dyn Fn(&WsMessage) -> WsFrameClass + Send + Sync>);

impl fmt::Debug for Classifier {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Classifier(..)")
  }
}

/// One WebSocket data message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsMessage {
  /// A text message.
  Text(String),
  /// A binary message.
  Binary(Bytes),
}

impl WsMessage {
  /// The text, for a text message.
  pub fn as_text(&self) -> Option<&str> {
    match self {
      WsMessage::Text(t) => Some(t),
      WsMessage::Binary(_) => None,
    }
  }

  /// The payload bytes.
  pub fn as_bytes(&self) -> &[u8] {
    match self {
      WsMessage::Text(t) => t.as_bytes(),
      WsMessage::Binary(b) => b,
    }
  }

  fn into_wire(self) -> Message {
    match self {
      WsMessage::Text(t) => Message::Text(t.into()),
      WsMessage::Binary(b) => Message::Binary(b),
    }
  }
}

impl From<String> for WsMessage {
  fn from(t: String) -> Self {
    WsMessage::Text(t)
  }
}

impl From<&str> for WsMessage {
  fn from(t: &str) -> Self {
    WsMessage::Text(t.to_owned())
  }
}

impl From<Bytes> for WsMessage {
  fn from(b: Bytes) -> Self {
    WsMessage::Binary(b)
  }
}

impl From<Vec<u8>> for WsMessage {
  fn from(b: Vec<u8>) -> Self {
    WsMessage::Binary(Bytes::from(b))
  }
}

/// The socket is closed: [`WsSender::send`] cannot deliver any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WsClosed;

impl fmt::Display for WsClosed {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("the websocket is closed")
  }
}

impl std::error::Error for WsClosed {}

/// The outbound half of a socket. Cheap to clone; [`send`](Self::send) suspends while the
/// outbox is full (backpressure from a slow client).
#[derive(Clone, Debug)]
pub struct WsSender {
  tx: mpsc::Sender<WsMessage>,
}

impl WsSender {
  /// Queues `msg` for the client, waiting while the outbox is full. Fails with [`WsClosed`] once
  /// the socket is closed.
  pub fn send<R>(&self, msg: impl Into<WsMessage>) -> Effect<(), WsClosed, R>
  where
    R: Send + Sync + 'static,
  {
    let tx = self.tx.clone();
    let msg = msg.into();
    from_async(move || async move { tx.send(msg).await.map_err(|_| WsClosed) })
  }

  /// A [`Sink`] sending every element (`stream.run(sender.sink())`).
  pub fn sink<R>(&self) -> Sink<(), WsMessage, WsClosed, R>
  where
    R: Send + Sync + 'static,
  {
    let this = self.clone();
    Sink::for_each(move |msg| this.send(msg))
  }

  /// `true` once the socket is closed.
  pub fn is_closed(&self) -> bool {
    self.tx.is_closed()
  }
}

/// What a WebSocket handler receives: the inbound stream, the [`WsSender`], and the
/// connection's subprotocol and client address.
#[derive(Debug)]
pub struct WsSocket {
  rx: mpsc::Receiver<WsMessage>,
  sender: WsSender,
  protocol: Option<String>,
  client_ip: Option<IpAddr>,
}

impl WsSocket {
  /// A socket whose messages another transport carries (r12-e12-3-webtransport: the frames of
  /// a WebTransport session's streams): `inbox` yields what the peer sent and ends when the
  /// peer is gone; `outbox` takes what the handler sends. No WebSocket framing, limits or
  /// heartbeats apply: they are the carrying transport's.
  pub fn from_channels(
    inbox: mpsc::Receiver<WsMessage>,
    outbox: mpsc::Sender<WsMessage>,
    protocol: Option<String>,
    client_ip: Option<IpAddr>,
  ) -> Self {
    Self {
      rx: inbox,
      sender: WsSender { tx: outbox },
      protocol,
      client_ip,
    }
  }

  /// The selected subprotocol (see [`WsConfig::protocols`]).
  pub fn protocol(&self) -> Option<&str> {
    self.protocol.as_deref()
  }

  /// The client's address, when the server records connect info.
  pub fn client_ip(&self) -> Option<IpAddr> {
    self.client_ip
  }

  /// Another handle to the outbound half.
  pub fn sender(&self) -> WsSender {
    self.sender.clone()
  }

  /// The inbound messages as a pull-based stream (it ends when the client closes or the socket
  /// is closed; each pull takes what is queued, at least one message) and the outbound half.
  /// Not pulling stops reading the connection once the inbox is full.
  pub fn into_parts<E, R>(self) -> (Stream<WsMessage, E, R>, WsSender)
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let rx = Arc::new(tokio::sync::Mutex::new(self.rx));
    let incoming = Stream::from_pull(move |max| {
      let rx = Arc::clone(&rx);
      from_async(move || async move {
        let mut rx = rx.lock().await;
        let Some(first) = rx.recv().await else {
          return Ok(None);
        };
        let mut out = vec![first];
        while out.len() < max {
          match rx.try_recv() {
            Ok(msg) => out.push(msg),
            Err(_) => break,
          }
        }
        Ok(Some(out))
      })
    });
    (incoming, self.sender)
  }
}

/// Why a socket ended, as the driver saw it (returned by [`serve_io`], logged by
/// [`endpoint`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WsClose {
  /// The handler returned; closed with 1000.
  HandlerDone,
  /// The handler failed or died; closed with 1011.
  HandlerFailed,
  /// The client sent a close frame (its code, if any); the close was echoed.
  ClientClosed(Option<u16>),
  /// The connection ended without a close frame (or a write timed out).
  ClientGone,
  /// No message either way for the idle timeout; closed with 1001.
  IdleTimeout,
  /// Too many pings went unanswered; closed with 1001.
  HeartbeatTimeout,
  /// A frame or message above the limit; closed with 1009.
  MessageTooBig,
  /// Inbound messages above the rate; closed with 1008.
  RateLimited,
  /// Inbound control frames above their own cap ([`WsConfig::control_messages_per_second`]);
  /// closed with 1008.
  ControlRateLimited,
  /// The client broke the protocol; closed with 1002.
  ProtocolError,
  /// A text message that is not UTF-8; closed with 1007.
  InvalidData,
}

impl WsClose {
  /// The close code the server sent, if it sent one.
  pub fn code(&self) -> Option<u16> {
    self.frame().map(|(code, _)| u16::from(code))
  }

  fn frame(&self) -> Option<(CloseCode, &'static str)> {
    Some(match self {
      WsClose::HandlerDone => (CloseCode::Normal, ""),
      WsClose::HandlerFailed => (CloseCode::Error, "internal error"),
      WsClose::IdleTimeout => (CloseCode::Away, "idle timeout"),
      WsClose::HeartbeatTimeout => (CloseCode::Away, "heartbeat timeout"),
      WsClose::MessageTooBig => (CloseCode::Size, "message too big"),
      WsClose::RateLimited => (CloseCode::Policy, "message rate exceeded"),
      WsClose::ControlRateLimited => (CloseCode::Policy, "control frame rate exceeded"),
      WsClose::ProtocolError => (CloseCode::Protocol, "protocol error"),
      WsClose::InvalidData => (CloseCode::Invalid, "invalid utf-8"),
      WsClose::ClientClosed(_) | WsClose::ClientGone => return None,
    })
  }
}

/// A GCRA meter: `units` per second, bursts of up to `units`.
#[derive(Clone, Copy, Debug)]
struct Rate {
  interval: Duration,
  tolerance: Duration,
  tat: Option<Instant>,
}

impl Rate {
  fn per_second(units: u32) -> Self {
    let interval = Duration::from_secs(1) / units.max(1);
    Self {
      interval,
      tolerance: Duration::from_secs(1).saturating_sub(interval),
      tat: None,
    }
  }

  fn admit(&mut self, now: Instant) -> bool {
    let tat = self.tat.map_or(now, |tat| tat.max(now));
    if tat > now + self.tolerance {
      return false;
    }
    self.tat = Some(tat + self.interval);
    true
  }
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type Reserve = BoxFuture<Result<mpsc::OwnedPermit<WsMessage>, mpsc::error::SendError<()>>>;

/// What woke the driver.
enum Event {
  Frame(Message),
  Error(WireError),
  Gone,
  HandlerDone(bool),
  Tick,
}

/// Owns the wire of one socket: reads frames into the inbox, writes the outbox, pings, timers
/// and limits, all on the executor clock.
struct Driver<T> {
  ws: WebSocketStream<T>,
  cfg: Arc<WsConfig>,
  exec: Arc<dyn Executor>,
  inbox: mpsc::Sender<WsMessage>,
  reserve: Option<Reserve>,
  /// An inbound message waiting for room in the inbox; the connection is not read meanwhile.
  pending: Option<WsMessage>,
  outbox: mpsc::Receiver<WsMessage>,
  outbox_open: bool,
  done: oneshot::Receiver<bool>,
  rate: Rate,
  /// Control frames' own meter (pings, pongs, classified control messages).
  control: Rate,
  awaiting_pong: bool,
  missed: u32,
  next_ping: Instant,
  last_activity: Instant,
  timer: Option<(Instant, BoxFuture<()>)>,
}

/// `fut`, unless `d` passes first on `exec`'s clock.
async fn within<F: Future>(exec: &Arc<dyn Executor>, d: Duration, fut: F) -> Option<F::Output> {
  let mut timer = exec.sleep(d);
  let mut fut = std::pin::pin!(fut);
  poll_fn(|cx| {
    if let Poll::Ready(out) = fut.as_mut().poll(cx) {
      return Poll::Ready(Some(out));
    }
    timer.as_mut().poll(cx).map(|()| None)
  })
  .await
}

impl<T> Driver<T>
where
  T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
  fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<Event> {
    if let Poll::Ready(ok) = Pin::new(&mut self.done).poll(cx) {
      return Poll::Ready(Event::HandlerDone(ok.unwrap_or(false)));
    }
    // Hand the pending message to the handler once the inbox has room.
    if self.pending.is_some() {
      let inbox = &self.inbox;
      let reserve = self
        .reserve
        .get_or_insert_with(|| Box::pin(inbox.clone().reserve_owned()));
      if let Poll::Ready(permit) = reserve.as_mut().poll(cx) {
        self.reserve = None;
        let msg = self.pending.take();
        // An inbox nobody reads any more drops the message.
        if let (Ok(permit), Some(msg)) = (permit, msg) {
          permit.send(msg);
        }
      }
    }
    // Write what the handler queued, as far as the connection takes it.
    while self.outbox_open {
      match Pin::new(&mut self.ws).poll_ready(cx) {
        Poll::Ready(Ok(())) => {}
        Poll::Ready(Err(_)) => return Poll::Ready(Event::Gone),
        Poll::Pending => break,
      }
      match self.outbox.poll_recv(cx) {
        Poll::Ready(Some(msg)) => {
          if Pin::new(&mut self.ws).start_send(msg.into_wire()).is_err() {
            return Poll::Ready(Event::Gone);
          }
          self.last_activity = self.exec.now();
        }
        Poll::Ready(None) => self.outbox_open = false,
        Poll::Pending => break,
      }
    }
    if let Poll::Ready(Err(_)) = Pin::new(&mut self.ws).poll_flush(cx) {
      return Poll::Ready(Event::Gone);
    }
    if self.pending.is_none() {
      match self.ws.poll_next_unpin(cx) {
        Poll::Ready(Some(Ok(msg))) => return Poll::Ready(Event::Frame(msg)),
        Poll::Ready(Some(Err(e))) => return Poll::Ready(Event::Error(e)),
        Poll::Ready(None) => return Poll::Ready(Event::Gone),
        Poll::Pending => {}
      }
    }
    // One timer for the earliest deadline; a later deadline (activity moved the idle deadline)
    // is re-checked when it fires.
    let deadline = self
      .next_ping
      .min(self.last_activity + self.cfg.idle_timeout);
    let rearm = match &self.timer {
      None => true,
      Some((at, _)) => deadline < *at,
    };
    if rearm {
      let wait = deadline.saturating_duration_since(self.exec.now());
      self.timer = Some((deadline, self.exec.sleep(wait)));
    }
    if let Some((_, timer)) = &mut self.timer
      && timer.as_mut().poll(cx).is_ready()
    {
      self.timer = None;
      return Poll::Ready(Event::Tick);
    }
    Poll::Pending
  }

  async fn run(mut self) -> WsClose {
    loop {
      let event = poll_fn(|cx| self.poll_event(cx)).await;
      let now = self.exec.now();
      let close = match event {
        Event::Frame(Message::Text(text)) => self.inbound(now, WsMessage::Text(text.to_string())),
        Event::Frame(Message::Binary(bytes)) => self.inbound(now, WsMessage::Binary(bytes)),
        Event::Frame(Message::Ping(_) | Message::Pong(_)) => {
          if self.control.admit(now) {
            self.awaiting_pong = false;
            self.missed = 0;
            None
          } else {
            Some(WsClose::ControlRateLimited)
          }
        }
        Event::Frame(Message::Close(frame)) => {
          // tungstenite queued the echo; flush it, bounded.
          let code = frame.map(|f| u16::from(f.code));
          let close_timeout = self.cfg.close_timeout;
          let _ = within(&self.exec, close_timeout, self.ws.flush()).await;
          return WsClose::ClientClosed(code);
        }
        Event::Frame(Message::Frame(_)) => None,
        Event::Error(e) => Some(match e {
          // EOF without a close frame: the client is gone.
          WireError::Protocol(ProtocolError::ResetWithoutClosingHandshake) => WsClose::ClientGone,
          WireError::Capacity(_) => WsClose::MessageTooBig,
          WireError::Utf8(_) => WsClose::InvalidData,
          WireError::Protocol(_) => WsClose::ProtocolError,
          _ => WsClose::ClientGone,
        }),
        Event::Gone => Some(WsClose::ClientGone),
        Event::HandlerDone(ok) => {
          self.drain().await;
          Some(if ok {
            WsClose::HandlerDone
          } else {
            WsClose::HandlerFailed
          })
        }
        Event::Tick => self.tick(now).await,
      };
      if let Some(close) = close {
        return self.close(close).await;
      }
    }
  }

  /// An inbound data message: rate-checked (on the meter of its class), then queued for the
  /// handler.
  fn inbound(&mut self, now: Instant, msg: WsMessage) -> Option<WsClose> {
    let (meter, limited) = match self.cfg.frame_class(&msg) {
      WsFrameClass::Data => (&mut self.rate, WsClose::RateLimited),
      WsFrameClass::Control => (&mut self.control, WsClose::ControlRateLimited),
    };
    if !meter.admit(now) {
      return Some(limited);
    }
    self.last_activity = now;
    self.awaiting_pong = false;
    self.missed = 0;
    self.pending = Some(msg);
    None
  }

  /// The timer fired: idle timeout, then the heartbeat.
  async fn tick(&mut self, now: Instant) -> Option<WsClose> {
    if now >= self.last_activity + self.cfg.idle_timeout {
      return Some(WsClose::IdleTimeout);
    }
    if now < self.next_ping {
      return None;
    }
    // While the handler holds the reads back (full inbox), pongs cannot be seen: no miss.
    if self.awaiting_pong && self.pending.is_none() {
      self.missed += 1;
      if self.missed >= self.cfg.missed_pongs {
        return Some(WsClose::HeartbeatTimeout);
      }
    }
    self.next_ping = now + self.cfg.ping_interval;
    self.awaiting_pong = true;
    let close_timeout = self.cfg.close_timeout;
    match within(
      &self.exec,
      close_timeout,
      self.ws.send(Message::Ping(Bytes::new())),
    )
    .await
    {
      Some(Ok(())) => None,
      _ => Some(WsClose::ClientGone),
    }
  }

  /// Writes what the handler queued before it returned.
  async fn drain(&mut self) {
    let close_timeout = self.cfg.close_timeout;
    while let Ok(msg) = self.outbox.try_recv() {
      if !matches!(
        within(&self.exec, close_timeout, self.ws.feed(msg.into_wire())).await,
        Some(Ok(()))
      ) {
        return;
      }
    }
    let _ = within(&self.exec, close_timeout, self.ws.flush()).await;
  }

  /// Sends the close frame for `close` (if it has one), bounded, and ends the connection.
  async fn close(mut self, close: WsClose) -> WsClose {
    if let Some((code, reason)) = close.frame() {
      let frame = CloseFrame {
        code,
        reason: reason.into(),
      };
      let close_timeout = self.cfg.close_timeout;
      let _ = within(
        &self.exec,
        close_timeout,
        self.ws.send(Message::Close(Some(frame))),
      )
      .await;
    }
    close
  }
}

/// Serves a WebSocket session on `io`, a connection whose HTTP upgrade is already done (the
/// `101` was sent): runs `handler` on a [`WsSocket`] as a child fiber beside the connection
/// driver, and returns why the session ended once the handler has finished (or been
/// interrupted, finalizers run).
///
/// [`endpoint`] calls this for every accepted upgrade; use it directly for other transports
/// (an HTTP/2 extended CONNECT stream, an in-memory pipe in tests). Timers use the running
/// fiber's executor clock.
pub fn serve_io<T, E, F>(io: T, config: &WsConfig, handler: F) -> Effect<WsClose, Never, HNil>
where
  T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  E: Send + 'static,
  F: FnOnce(WsSocket) -> Effect<(), E, HNil> + Send + 'static,
{
  session(io, Arc::new(config.clone()), None, None, handler)
}

fn session<T, E, F>(
  io: T,
  cfg: Arc<WsConfig>,
  protocol: Option<String>,
  client_ip: Option<IpAddr>,
  handler: F,
) -> Effect<WsClose, Never, HNil>
where
  T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
  E: Send + 'static,
  F: FnOnce(WsSocket) -> Effect<(), E, HNil> + Send + 'static,
{
  let wire = cfg.wire();
  from_async(move || async move {
    Ok::<_, Never>(WebSocketStream::from_raw_socket(io, Role::Server, Some(wire)).await)
  })
  .flat_map(move |ws| {
    let (in_tx, in_rx) = mpsc::channel(cfg.inbox);
    let (out_tx, out_rx) = mpsc::channel(cfg.outbox);
    let (done_tx, done_rx) = oneshot::channel::<bool>();
    let socket = WsSocket {
      rx: in_rx,
      sender: WsSender { tx: out_tx },
      protocol,
      client_ip,
    };
    let handler = suspend(move || handler(socket))
      .exit::<Never>()
      .map(move |exit| {
        let _ = done_tx.send(matches!(exit, Exit::Success(())));
      });
    handler.fork().flat_map(move |fiber| {
      Effect::from_ctx(move |ctx: Ctx<HNil>| {
        let exec = Arc::clone(ctx.fiber().executor());
        async move {
          let now = exec.now();
          let driver = Driver {
            ws,
            exec,
            inbox: in_tx,
            reserve: None,
            pending: None,
            outbox: out_rx,
            outbox_open: true,
            done: done_rx,
            rate: Rate::per_second(cfg.messages_per_second),
            control: Rate::per_second(cfg.control_message_rate()),
            awaiting_pong: false,
            missed: 0,
            next_ping: now + cfg.ping_interval,
            last_activity: now,
            timer: None,
            cfg,
          };
          Ok::<_, Never>(driver.run().await)
        }
      })
      // The wire is gone (or the handler is done): interrupt the handler, finalizers run.
      .flat_map(move |close| fiber.interrupt::<Never, HNil>().map(move |_| close))
    })
  })
}

/// Whether the comma-separated header `name` holds `token` (case-insensitive).
fn has_token(headers: &HeaderMap, name: header::HeaderName, token: &str) -> bool {
  headers
    .get_all(name)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .any(|t| t.trim().eq_ignore_ascii_case(token))
}

fn upgrade_required(detail: &str, extra: Option<(HeaderName, &'static str)>) -> Response {
  let mut res = Problem::new(StatusCode::UPGRADE_REQUIRED)
    .with_title("WebSocket upgrade required")
    .with_detail(detail)
    .into_response();
  res
    .headers_mut()
    .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
  if let Some((name, value)) = extra {
    res
      .headers_mut()
      .insert(name, HeaderValue::from_static(value));
  }
  res
}

/// Checks the RFC 6455 handshake headers; the client key on success.
#[allow(clippy::result_large_err)]
fn handshake(headers: &HeaderMap) -> Result<HeaderValue, Response> {
  if !has_token(headers, header::CONNECTION, "upgrade")
    || !has_token(headers, header::UPGRADE, "websocket")
  {
    return Err(upgrade_required(
      "this endpoint only serves WebSocket upgrades",
      None,
    ));
  }
  if headers
    .get(SEC_WEBSOCKET_VERSION)
    .is_none_or(|v| v.as_bytes() != b"13")
  {
    return Err(upgrade_required(
      "Sec-WebSocket-Version must be 13",
      Some((SEC_WEBSOCKET_VERSION, "13")),
    ));
  }
  match headers.get(SEC_WEBSOCKET_KEY) {
    Some(key)
      if base64::engine::general_purpose::STANDARD
        .decode(key.as_bytes())
        .is_ok_and(|k| k.len() == 16) =>
    {
      Ok(key.clone())
    }
    _ => Err(
      Problem::new(StatusCode::BAD_REQUEST)
        .with_title("Invalid WebSocket handshake")
        .with_detail("Sec-WebSocket-Key must be 16 bytes in base64")
        .into_response(),
    ),
  }
}

/// The first subprotocol of `ours` the client offers.
fn select_protocol(headers: &HeaderMap, ours: &[String]) -> Option<String> {
  if ours.is_empty() {
    return None;
  }
  let offered: Vec<&str> = headers
    .get_all(SEC_WEBSOCKET_PROTOCOL)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .map(str::trim)
    .collect();
  ours
    .iter()
    .find(|p| offered.iter().any(|o| o == p))
    .cloned()
}

/// The shared state of one endpoint.
struct Shared {
  config: Arc<WsConfig>,
  cap: ConnectionCap,
}

/// Answers one upgrade request: handshake, Origin, connection cap, then `101` and the session
/// fiber.
fn upgrade<S, E, F>(shared: &Shared, env: S, mut req: Request, f: F) -> Response
where
  S: Send + Sync + 'static,
  E: Send + 'static,
  F: FnOnce(&mut S, RequestCaps, WsSocket) -> Effect<(), E, S> + Send + 'static,
{
  let key = match handshake(req.headers()) {
    Ok(key) => key,
    Err(res) => return res,
  };
  if let Err(rejection) = shared.config.origins.check(req.headers()) {
    tracing::debug!(target: "id_effect_axum::realtime::ws", %rejection, "websocket upgrade refused");
    return rejection.into_response();
  }
  let protocol = select_protocol(req.headers(), &shared.config.protocols);
  let none = TrustedProxies::none();
  let trusted = req.extensions().get::<TrustedProxies>().unwrap_or(&none);
  let client_ip = ClientIp::from_parts(req.extensions(), req.headers(), trusted).map(|c| c.0);
  let permit = match shared.cap.acquire(client_ip) {
    Ok(permit) => permit,
    Err(reached) => return reached.into_response(),
  };
  if req
    .extensions()
    .get::<hyper::upgrade::OnUpgrade>()
    .is_none()
  {
    return upgrade_required("the connection cannot be upgraded", None);
  }
  let on_upgrade = hyper::upgrade::on(&mut req);
  let caps = req
    .extensions()
    .get::<RequestCaps>()
    .cloned()
    .unwrap_or_default();
  let cfg = Arc::clone(&shared.config);
  let selected = protocol.clone();
  let fiber = from_async(move || async move { Ok::<_, Never>(on_upgrade.await) })
    .flat_map(move |upgraded| match upgraded {
      Err(error) => {
        tracing::debug!(target: "id_effect_axum::realtime::ws", %error, "websocket upgrade failed");
        succeed(WsClose::ClientGone)
      }
      Ok(upgraded) => session(
        TokioIo::new(upgraded),
        cfg,
        selected,
        client_ip,
        move |socket| crate::executor::request_effect(env, move |s| f(s, caps, socket)),
      ),
    })
    .map(|close| {
      tracing::debug!(target: "id_effect_axum::realtime::ws", ?close, "websocket closed");
    })
    .ensuring(sync(move || drop(permit)));
  drop(crate::EffectExecutor::global().runtime().fork(fiber));
  let mut res = Response::new(Body::empty());
  *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
  let headers = res.headers_mut();
  headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
  headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
  let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
  if let Ok(accept) = HeaderValue::from_str(&accept) {
    headers.insert(SEC_WEBSOCKET_ACCEPT, accept);
  }
  if let Some(protocol) = protocol.and_then(|p| HeaderValue::from_str(&p).ok()) {
    headers.insert(SEC_WEBSOCKET_PROTOCOL, protocol);
  }
  res
}

/// A typed `GET` route at `path` serving WebSocket upgrades with `handler` (see the
/// [module docs](self)). Decide its access with `.public()` or `.guard(g)`; the guards (and an
/// `Authenticate` layer) run on the upgrade request, before the `101`.
///
/// Per socket, `handler(&mut state, caps, socket)` builds the effect that serves it, with the
/// upgrade request's [`RequestCaps`]. Sockets are counted against [`WsConfig::max_sockets`]
/// (or [`WsConfig::connection_cap`]).
pub fn endpoint<S, E, F>(
  path: impl Into<String>,
  config: WsConfig,
  handler: F,
) -> Endpoint<S, Undecided>
where
  S: Clone + Send + Sync + 'static,
  E: Send + 'static,
  F: Fn(&mut S, RequestCaps, WsSocket) -> Effect<(), E, S> + Clone + Send + Sync + 'static,
{
  let shared = Arc::new(Shared {
    cap: config.cap(),
    config: Arc::new(config),
  });
  let mut info = RouteInfo::new(Method::GET, path);
  info.handler = Some(std::any::type_name::<F>());
  let method_router = axum::routing::get(move |State(env): State<S>, req: Request| {
    let shared = Arc::clone(&shared);
    let f = handler.clone();
    async move { upgrade(&shared, env, req, f) }
  });
  Endpoint::from_parts(info, method_router)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
      h.append(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    h
  }

  const GOOD: [(&str, &str); 4] = [
    ("connection", "keep-alive, Upgrade"),
    ("upgrade", "WebSocket"),
    ("sec-websocket-version", "13"),
    ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
  ];

  #[test]
  fn handshake_checks_upgrade_version_and_key() {
    assert!(handshake(&headers(&GOOD)).is_ok());
    let res = handshake(&headers(&GOOD[1..])).unwrap_err();
    assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
    let mut v12 = GOOD;
    v12[2] = ("sec-websocket-version", "12");
    let res = handshake(&headers(&v12)).unwrap_err();
    assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(res.headers()[SEC_WEBSOCKET_VERSION], "13");
    let mut short = GOOD;
    short[3] = ("sec-websocket-key", "c2hvcnQ=");
    assert_eq!(
      handshake(&headers(&short)).unwrap_err().status(),
      StatusCode::BAD_REQUEST
    );
  }

  #[test]
  fn protocols_pick_ours_in_order() {
    let h = headers(&[
      ("sec-websocket-protocol", "b, a"),
      ("sec-websocket-protocol", "c"),
    ]);
    let ours = vec!["a".to_owned(), "b".to_owned()];
    assert_eq!(select_protocol(&h, &ours).as_deref(), Some("a"));
    assert_eq!(select_protocol(&h, &[]), None);
    assert_eq!(select_protocol(&h, &["z".to_owned()]), None);
  }

  #[test]
  fn config_clamps_and_reports() {
    let c = WsConfig::new()
      .max_message_bytes(1, 2)
      .messages_per_second(0)
      .heartbeat(Duration::ZERO, 0)
      .idle_timeout(Duration::ZERO)
      .close_timeout(Duration::ZERO)
      .max_sockets(0, 0)
      .buffers(0, 0)
      .protocols(["p"]);
    assert_eq!((c.max_frame(), c.max_message()), (125, 125));
    assert_eq!(c.message_rate(), 1);
    // The control cap follows the message rate until it is set.
    assert_eq!(c.control_message_rate(), DEFAULT_CONTROL_RATE_FACTOR);
    assert_eq!(c.frame_class(&WsMessage::from("x")), WsFrameClass::Data);
    let c = c
      .control_messages_per_second(0)
      .classify(|_| WsFrameClass::Control);
    assert_eq!(c.control_message_rate(), 1);
    assert_eq!(c.frame_class(&WsMessage::from("x")), WsFrameClass::Control);
    assert!(format!("{c:?}").contains("Classifier(..)"));
    assert_eq!(
      WsConfig::new()
        .messages_per_second(u32::MAX)
        .control_message_rate(),
      u32::MAX
    );
    assert_eq!(c.heartbeat_settings(), (Duration::from_millis(1), 1));
    assert_eq!(c.idle(), Duration::from_millis(1));
    assert_eq!(c.socket_limits(), (1, 1));
    assert_eq!((c.inbox, c.outbox), (1, 1));
    let shared = ConnectionCap::new(7, 3);
    let c = c.connection_cap(shared);
    assert_eq!(c.socket_limits(), (7, 3));
    assert_eq!(c.cap().max_total(), 7);
    let d = WsConfig::default();
    assert_eq!((d.max_frame(), d.max_message()), (65_536, 65_536));
    assert_eq!(d.heartbeat_settings(), (Duration::from_secs(20), 2));
    assert!(!d.origin_policy().is_any());
    assert!(WsConfig::new().allowed_origins(["nope"]).is_err());
  }

  #[test]
  fn messages_convert() {
    assert_eq!(WsMessage::from("a").as_text(), Some("a"));
    assert_eq!(WsMessage::from(String::from("b")).as_bytes(), b"b");
    let bin = WsMessage::from(vec![1_u8, 2]);
    assert_eq!(bin.as_text(), None);
    assert_eq!(bin.as_bytes(), [1, 2]);
    assert_eq!(WsMessage::from(Bytes::from_static(b"x")).as_bytes(), b"x");
    assert_eq!(WsClosed.to_string(), "the websocket is closed");
    assert_eq!(WsClose::MessageTooBig.code(), Some(1009));
    assert_eq!(WsClose::RateLimited.code(), Some(1008));
    assert_eq!(WsClose::ControlRateLimited.code(), Some(1008));
    assert_eq!(WsClose::ProtocolError.code(), Some(1002));
    assert_eq!(WsClose::InvalidData.code(), Some(1007));
    assert_eq!(WsClose::HandlerFailed.code(), Some(1011));
    assert_eq!(WsClose::ClientGone.code(), None);
  }

  #[test]
  fn rate_admits_a_burst_then_the_rate() {
    let t0 = Instant::now();
    let mut r = Rate::per_second(2);
    assert!(r.admit(t0) && r.admit(t0));
    assert!(!r.admit(t0));
    assert!(r.admit(t0 + Duration::from_millis(500)));
  }

  type Slot<T> = Arc<std::sync::Mutex<Option<T>>>;
  type Client = WebSocketStream<tokio::io::DuplexStream>;

  /// Runs `fut` as a fiber on `exec` until it stalls; it must have completed by then.
  fn drive<T, F>(exec: &id_effect::TestExecutor, fut: F) -> T
  where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
  {
    let slot: Slot<T> = Arc::new(std::sync::Mutex::new(None));
    let out = Arc::clone(&slot);
    let eff: Effect<(), Never, HNil> = from_async(move || async move {
      let v = fut.await;
      *out.lock().unwrap() = Some(v);
      Ok(())
    });
    drop(eff.fork_root(Arc::new(exec.clone())));
    exec.run_until_stalled();
    let v = slot.lock().unwrap().take();
    v.expect("the step completed")
  }

  /// A session on the test clock whose handler drops every message, and the client end.
  fn session(exec: &id_effect::TestExecutor, config: &WsConfig) -> (Slot<WsClose>, Client) {
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let closed: Slot<WsClose> = Arc::new(std::sync::Mutex::new(None));
    let out = Arc::clone(&closed);
    let server = serve_io(server_io, config, |socket: WsSocket| {
      let (incoming, _out) = socket.into_parts::<Never, HNil>();
      incoming.run_for_each_effect(|_| succeed(()))
    })
    .map(move |close| *out.lock().unwrap() = Some(close));
    drop(server.fork_root(Arc::new(exec.clone())));
    let client = drive(exec, async move {
      WebSocketStream::from_raw_socket(client_io, Role::Client, None).await
    });
    (closed, client)
  }

  /// Sends `msgs` at one instant, then reads up to the server's close frame (its code and
  /// reason; `None` when the server did not close).
  fn send_all(
    exec: &id_effect::TestExecutor,
    mut client: Client,
    msgs: Vec<Message>,
    expect_close: bool,
  ) -> (Client, Option<(u16, String)>) {
    use futures_util::{SinkExt as _, StreamExt as _};
    drive(exec, async move {
      for msg in msgs {
        client.send(msg).await.unwrap();
      }
      let mut close = None;
      while expect_close && let Some(Ok(msg)) = client.next().await {
        if let Message::Close(Some(frame)) = msg {
          close = Some((u16::from(frame.code), frame.reason.to_string()));
          break;
        }
      }
      (client, close)
    })
  }

  #[test]
  fn r4_classified_control_frames_have_own_cap() {
    let exec = id_effect::TestExecutor::new();
    let config = WsConfig::default().messages_per_second(2).classify(|msg| {
      if msg.as_text() == Some("credit") {
        WsFrameClass::Control
      } else {
        WsFrameClass::Data
      }
    });
    assert_eq!(config.control_message_rate(), 8);
    let (closed, client) = session(&exec, &config);
    // At one instant: the two data messages the rate allows, and eight control frames (six
    // classified credits, a ping and a pong) beside them.
    let mut burst = vec![Message::text("a"), Message::text("b")];
    burst.extend((0..6).map(|_| Message::text("credit")));
    burst.extend([Message::Ping(Bytes::new()), Message::Pong(Bytes::new())]);
    let (client, close) = send_all(&exec, client, burst, false);
    assert_eq!(close, None);
    assert!(
      closed.lock().unwrap().is_none(),
      "control frames left the data rate"
    );
    // A ninth control frame at the same instant is over the control cap.
    let (_client, close) = send_all(&exec, client, vec![Message::text("credit")], true);
    assert_eq!(close, Some((1008, "control frame rate exceeded".into())));
    exec.run_until_stalled();
    assert_eq!(
      closed.lock().unwrap().take(),
      Some(WsClose::ControlRateLimited)
    );
  }

  #[test]
  fn r4_ping_flood_closes_1008() {
    let exec = id_effect::TestExecutor::new();
    let config = WsConfig::default().control_messages_per_second(3);
    let (closed, client) = session(&exec, &config);
    let pings = (0..3).map(|_| Message::Ping(Bytes::new())).collect();
    let (client, close) = send_all(&exec, client, pings, false);
    assert_eq!(close, None);
    // The cap refills at its rate.
    exec.advance(Duration::from_secs(1));
    let pings = (0..3).map(|_| Message::Ping(Bytes::new())).collect();
    let (client, close) = send_all(&exec, client, pings, false);
    assert_eq!(close, None);
    assert!(closed.lock().unwrap().is_none());
    let (_client, close) = send_all(&exec, client, vec![Message::Ping(Bytes::new())], true);
    assert_eq!(close, Some((1008, "control frame rate exceeded".into())));
    exec.run_until_stalled();
    assert_eq!(
      closed.lock().unwrap().take(),
      Some(WsClose::ControlRateLimited)
    );
  }
}
