//! HTTP/3 into the axum [`Router`] (feature `http3`, ADR 0014 "HTTP/3"): requests arrive over
//! `h3` + `h3-quinn` from the platform [`QuicEndpoint`] (ALPN `h3`) and go to the **same**
//! router the TCP listener serves, as a tower `Service`, so every typed route, middleware,
//! guard and RPC endpoint works over HTTP/3 unchanged. This module is the whole adapter, kept
//! thin so the backend can be swapped once hyper ships HTTP/3.
//!
//! The host entry points ([`ServerHandle`](crate::server::ServerHandle),
//! [`serve_router`](crate::server::serve_router), [`server_layer`](crate::server::server_layer))
//! serve HTTP/3 by themselves when [`HostConfig::http3`](crate::server::HostConfig::http3) is on
//! (the default) and the host has TLS from certificate files: UDP binds the TCP port number,
//! TCP responses carry `Alt-Svc: h3=":<port>"; ma=86400`, and shutdown drains both. `HTTP3=false`
//! (one flag) turns HTTP/3 and `Alt-Svc` off. [`serve_h3`] is the building block for a QUIC
//! endpoint of your own (one shared with the mesh, say).
//!
//! ## Limits (ADR 0013 R4)
//!
//! The [`ConnectionLimits`] of the TCP listener apply to HTTP/3 as well:
//!
//! - **connections**: at most [`ConnectionLimits::max_connections`] HTTP/3 connections at once
//!   (a pool of its own, the same size as TCP's); further connections wait in the endpoint's
//!   route backlog, which refuses them once full;
//! - **request header timeout**: the first request of a connection, and each request's header
//!   section, must arrive within [`ConnectionLimits::header_read_timeout`];
//! - **requests per connection**: the `n`th request starts a graceful close (`GOAWAY`), the
//!   requests already open finish;
//! - **QUIC stream credits**: a client may open at most
//!   [`ConnectionLimits::max_concurrent_streams`] requests at once (capped at the endpoint
//!   ceiling of 100, see [`quic_limits`]); the TLS handshake timeout bounds the QUIC handshake.
//!
//! Request header sections are capped at 64 KiB. Request bodies are streamed to the router;
//! the body limits of the app (`DefaultBodyLimit`, request caps) apply as on TCP.
//!
//! ## Client IP
//!
//! Every request carries `ConnectInfo<SocketAddr>` with the QUIC peer address, so
//! [`ClientIp`](crate::ClientIp) applies the 0.10.1 trusted-proxy rules to HTTP/3 exactly as to
//! TCP, and per-IP rate limits see the same address.
//!
//! ## 0-RTT
//!
//! Off: the platform endpoint accepts no early data and hands connections over only after the
//! handshake. [`Http3Options::dangerous_allow_zero_rtt_for_queries`] records a posture opt-out
//! and admits early requests with safe methods only ([`Http3Options::admits_early`]); the
//! endpoint does not yet offer early data, so today it changes nothing on the wire.
//!
//! ```rust,no_run
//! use axum::Router;
//! use id_effect_axum::server::ConnectionLimits;
//! use id_effect_axum::server::h3::{Http3Options, quic_limits, serve_h3};
//! use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};
//! use id_effect_platform::tls::{Identity, NodeCertificate};
//!
//! # async fn run() -> std::io::Result<()> {
//! let identity = Identity::from_certificate(&NodeCertificate::self_signed("localhost").unwrap())
//!   .unwrap();
//! let limits = ConnectionLimits::default();
//! let endpoint = QuicEndpoint::bind(
//!   "127.0.0.1:8443".parse().unwrap(),
//!   QuicEndpointConfig::new(identity).with_limits(quic_limits(&limits)),
//! )
//! .unwrap();
//! serve_h3(&endpoint, Router::new(), limits, Http3Options::default(), async {
//!   let _ = tokio::signal::ctrl_c().await;
//! })
//! .await
//! # }
//! ```

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use ::h3::error::{Code, StreamError};
use ::h3::server::{RequestResolver, RequestStream};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Response, header};
use bytes::Buf;
use http_body::Frame;
use http_body_util::BodyExt;
use id_effect::observability::posture::record_dangerous;
use id_effect_platform::quic::{AlpnRoute, QuicEndpoint, QuicEndpointConfig, QuicLimits, Route};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tower_service::Service;

use crate::server::bootstrap::HostConfig;
use crate::server::connection::{ConnectionLimits, HostTransport};

/// Posture id of [`Http3Options::dangerous_allow_zero_rtt_for_queries`] (ADR 0013 R10).
pub const DANGEROUS_ALLOW_ZERO_RTT_FOR_QUERIES: &str =
  "id_effect_axum::server::h3::Http3Options::dangerous_allow_zero_rtt_for_queries";

/// Largest request header section accepted (64 KiB).
pub const MAX_FIELD_SECTION_SIZE: u64 = 64 * 1024;

/// Application close code of a connection whose first request did not arrive within the
/// header timeout (`H3_NO_ERROR`: nothing was lost).
const H3_NO_ERROR: u32 = 0x100;

pub(crate) type Connection = ::h3::server::Connection<h3_quinn::Connection, Bytes>;
type Resolver = RequestResolver<h3_quinn::Connection, Bytes>;
pub(crate) type SendHalf = RequestStream<h3_quinn::SendStream<Bytes>, Bytes>;
type RecvHalf = RequestStream<h3_quinn::RecvStream, Bytes>;
/// A request stream before it is split (what a WebTransport session takes over).
pub(crate) type FullStream = RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

/// What a request task hands its connection when a WebTransport session takes it over
/// (r12-e12-3-webtransport); nothing without the feature.
#[cfg(feature = "webtransport")]
pub(crate) type Handoff = crate::server::webtransport::Handoff;
#[cfg(not(feature = "webtransport"))]
pub(crate) type Handoff = std::convert::Infallible;

/// The next hand-off, or never (no WebTransport on this connection).
async fn next_handoff(handoffs: &mut Option<mpsc::Receiver<Handoff>>) -> Handoff {
  match handoffs {
    Some(rx) => match rx.recv().await {
      Some(handoff) => handoff,
      None => std::future::pending().await,
    },
    None => std::future::pending().await,
  }
}

/// HTTP/3 settings beyond the [`ConnectionLimits`]: off by default, each a `dangerous_*`
/// opt-out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Http3Options {
  zero_rtt_for_queries: bool,
  #[cfg(feature = "webtransport")]
  webtransport: bool,
}

impl Http3Options {
  /// The defaults: 0-RTT off.
  pub fn new() -> Self {
    Self::default()
  }

  /// **Dangerous.** Admit requests that arrive in 0-RTT early data when their method is safe
  /// (`GET`, `HEAD`, `OPTIONS`: RPC queries); every other method is never served from early
  /// data. Recorded in the [posture registry](id_effect::observability::posture) as
  /// [`DANGEROUS_ALLOW_ZERO_RTT_FOR_QUERIES`].
  ///
  /// Early data can be replayed by an attacker who captured it: only handlers that are
  /// idempotent **and** reveal nothing a replay could exploit may sit behind safe methods.
  /// The platform QUIC endpoint does not offer early data yet, so the opt-out has no effect on
  /// the wire until it does.
  #[must_use]
  pub fn dangerous_allow_zero_rtt_for_queries(mut self) -> Self {
    record_dangerous(
      DANGEROUS_ALLOW_ZERO_RTT_FOR_QUERIES,
      "R10: HTTP/3 0-RTT admitted for safe methods; early data is replayable",
    );
    self.zero_rtt_for_queries = true;
    self
  }

  /// Whether 0-RTT is admitted for safe methods.
  pub fn zero_rtt_for_queries(&self) -> bool {
    self.zero_rtt_for_queries
  }

  /// Serve WebTransport sessions (feature `webtransport`): the connections advertise extended
  /// CONNECT, HTTP/3 datagrams and one WebTransport session each, and a WebTransport CONNECT
  /// that a [`webtransport::endpoint`](crate::server::webtransport::endpoint) route accepts
  /// takes its connection over. Off by default. The endpoint needs QUIC datagrams
  /// ([`webtransport::quic_limits`](crate::server::webtransport::quic_limits)).
  #[cfg(feature = "webtransport")]
  #[must_use]
  pub fn webtransport(mut self) -> Self {
    self.webtransport = true;
    self
  }

  /// Whether WebTransport sessions are served.
  #[cfg(feature = "webtransport")]
  pub fn webtransport_enabled(&self) -> bool {
    self.webtransport
  }

  /// Whether the connections advertise and accept WebTransport.
  fn serves_webtransport(&self) -> bool {
    #[cfg(feature = "webtransport")]
    return self.webtransport;
    #[cfg(not(feature = "webtransport"))]
    false
  }

  /// Whether a request with `method` may be served from 0-RTT early data: only with
  /// [`Self::dangerous_allow_zero_rtt_for_queries`] and only for `GET`, `HEAD` and `OPTIONS`.
  pub fn admits_early(&self, method: &Method) -> bool {
    self.zero_rtt_for_queries && matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
  }
}

/// The endpoint limits matching `limits`: the platform defaults (the ceiling), with at most
/// [`ConnectionLimits::max_concurrent_streams`] bidirectional streams (requests) per connection
/// and the QUIC handshake bounded by [`ConnectionLimits::tls_handshake_timeout`] — both capped
/// at the endpoint defaults, which raising would need
/// `QuicEndpointConfig::dangerous_raise_limits`.
pub fn quic_limits(limits: &ConnectionLimits) -> QuicLimits {
  let mut quic = QuicLimits::default();
  if let Some(streams) = limits.concurrent_streams_limit() {
    quic.max_concurrent_bidi_streams = streams.clamp(1, quic.max_concurrent_bidi_streams);
  }
  if let Some(timeout) = limits.tls_handshake_timeout_limit() {
    quic.handshake_timeout = timeout.min(quic.handshake_timeout);
  }
  quic
}

fn io_error(e: impl std::fmt::Display) -> std::io::Error {
  std::io::Error::other(format!("http3: {e}"))
}

/// Serves `app` over HTTP/3 on `endpoint` (it registers the `h3` route) within `limits` until
/// `shutdown` completes; then every connection is closed gracefully (`GOAWAY`: open requests
/// finish, new ones are refused) and the function returns once the last connection has
/// closed. The route is released on return, so the endpoint refuses `h3` from then on.
///
/// # Errors
///
/// When the endpoint already routes `h3` or cannot take the route.
///
/// The route is registered when `serve_h3` is called, before the returned future first runs,
/// so a client may connect as soon as the call returns (also when the future is spawned).
pub fn serve_h3<F>(
  endpoint: &QuicEndpoint,
  app: Router,
  limits: ConnectionLimits,
  options: Http3Options,
  shutdown: F,
) -> impl Future<Output = std::io::Result<()>> + Send + 'static
where
  F: Future<Output = ()> + Send + 'static,
{
  let route = endpoint.register(Route::h3()).map_err(io_error);
  serve_route(route, app, limits, options, shutdown)
}

async fn serve_route<F>(
  route: std::io::Result<AlpnRoute>,
  app: Router,
  limits: ConnectionLimits,
  options: Http3Options,
  shutdown: F,
) -> std::io::Result<()>
where
  F: Future<Output = ()> + Send + 'static,
{
  let route = route?;
  // Dropping the receiver is the signal; connections watch the sender.
  let (signal_tx, signal_rx) = watch::channel(());
  tokio::spawn(async move {
    shutdown.await;
    drop(signal_rx);
  });
  // Every connection holds a receiver; the loop waits for the last one to drop.
  let (close_tx, close_rx) = watch::channel(());
  let limits = Arc::new(limits);
  // Connections are handed over after the handshake, so no request is early data and the
  // 0-RTT rule ([`Http3Options::admits_early`]) has nothing to decide yet.
  let _ = options.zero_rtt_for_queries();
  let slots = limits
    .connections_limit()
    .map(|n| Arc::new(Semaphore::new(n)));
  loop {
    let permit = match &slots {
      Some(slots) => tokio::select! {
        permit = Arc::clone(slots).acquire_owned() => permit.ok(),
        () = signal_tx.closed() => break,
      },
      None => None,
    };
    let conn = tokio::select! {
      accepted = route.accept() => match accepted {
        Some(Ok(conn)) => conn,
        // A refused handshake (reported for logs only).
        Some(Err(_)) => continue,
        None => break,
      },
      () = signal_tx.closed() => break,
    };
    let task = ConnectionTask {
      conn,
      app: app.clone(),
      limits: Arc::clone(&limits),
      signal: signal_tx.clone(),
      held: (close_rx.clone(), permit),
      webtransport: options.serves_webtransport(),
    };
    tokio::spawn(task.run());
  }
  drop(route);
  drop(close_rx);
  close_tx.closed().await;
  Ok(())
}

/// One QUIC connection and what it holds until it closes.
struct ConnectionTask {
  conn: quinn::Connection,
  app: Router,
  limits: Arc<ConnectionLimits>,
  signal: watch::Sender<()>,
  held: (watch::Receiver<()>, Option<OwnedSemaphorePermit>),
  /// Advertise WebTransport, and let a session take the connection over.
  webtransport: bool,
}

impl ConnectionTask {
  async fn run(self) {
    let Self {
      conn,
      app,
      limits,
      signal,
      held,
      webtransport,
    } = self;
    // Released when the connection is done, whichever way.
    let _held = held;
    let peer = conn.remote_address();
    let mut builder = ::h3::server::builder();
    builder.max_field_section_size(MAX_FIELD_SECTION_SIZE);
    if webtransport {
      builder
        .enable_webtransport(true)
        .enable_extended_connect(true)
        .enable_datagram(true)
        .max_webtransport_sessions(1);
    }
    let built = builder
      .build::<_, Bytes>(h3_quinn::Connection::new(conn.clone()))
      .await;
    // A request task whose CONNECT a WebTransport route accepted hands the stream back here:
    // the session takes the connection over (one per connection).
    let (handoff_tx, mut handoffs) = if webtransport {
      let (tx, rx) = mpsc::channel::<Handoff>(1);
      (Some(tx), Some(rx))
    } else {
      (None, None)
    };
    let Ok(mut h3) = built else {
      return;
    };
    let header_timeout = limits.header_read_timeout_limit();
    let max_requests = limits.requests_per_connection_limit();
    let mut first = std::pin::pin!(tokio::time::sleep(header_timeout.unwrap_or_default()));
    let mut armed = header_timeout.is_some();
    let mut served = 0usize;
    let mut draining = false;
    // The open requests: after our GOAWAY, h3 keeps accepting until the *peer* sends one, so
    // the drain ends when these are done.
    let mut requests = tokio::task::JoinSet::new();
    loop {
      tokio::select! {
        accepted = h3.accept() => match accepted {
          Ok(Some(resolver)) => {
            armed = false;
            served += 1;
            let request = RequestTask {
              resolver,
              app: app.clone(),
              peer,
              header_timeout,
              handoff: handoff_tx.clone(),
            };
            requests.spawn(request.run());
            if !draining && max_requests.is_some_and(|max| served >= max) {
              draining = true;
              let _ = shutdown(&mut h3).await;
            }
          }
          // The peer's GOAWAY with nothing open, or a closed connection.
          Ok(None) | Err(_) => return,
        },
        Some(_) = requests.join_next(), if !requests.is_empty() => {
          if draining && requests.is_empty() {
            break;
          }
        }
        handoff = next_handoff(&mut handoffs) => {
          // The open requests carry on by themselves.
          requests.detach_all();
          return take_over(h3, conn, handoff, app, peer, signal).await;
        }
        () = &mut first, if armed => {
          conn.close(H3_NO_ERROR.into(), b"no request within the header timeout");
          return;
        }
        () = signal.closed(), if !draining => {
          draining = true;
          let _ = shutdown(&mut h3).await;
          if requests.is_empty() {
            break;
          }
        }
      }
    }
    // Drained: give the client a moment to read the last responses and close by itself, then
    // close (dropping the h3 connection sends H3_NO_ERROR).
    let _ = tokio::time::timeout(GOAWAY_GRACE, conn.closed()).await;
  }
}

/// A WebTransport session takes the connection over until it ends (or the server shuts down).
#[cfg(feature = "webtransport")]
async fn take_over(
  h3: Connection,
  conn: quinn::Connection,
  handoff: Handoff,
  app: Router,
  peer: SocketAddr,
  signal: watch::Sender<()>,
) {
  crate::server::webtransport::drive(h3, conn, handoff, app, peer, signal).await;
}

#[cfg(not(feature = "webtransport"))]
async fn take_over(
  _: Connection,
  _: quinn::Connection,
  handoff: Handoff,
  _: Router,
  _: SocketAddr,
  _: watch::Sender<()>,
) {
  match handoff {}
}

/// How long a drained connection waits for the client to close before the server does.
const GOAWAY_GRACE: Duration = Duration::from_secs(1);

/// `GOAWAY` at the last accepted request: open requests finish, new ones are refused.
async fn shutdown(h3: &mut Connection) -> Result<(), ::h3::error::ConnectionError> {
  h3.shutdown(0).await
}

/// One request stream: its header section within the timeout, then the router, then the
/// response.
struct RequestTask {
  resolver: Resolver,
  app: Router,
  peer: SocketAddr,
  header_timeout: Option<Duration>,
  /// Where a WebTransport CONNECT goes once its route accepted it (WebTransport on).
  handoff: Option<mpsc::Sender<Handoff>>,
}

impl RequestTask {
  async fn run(self) {
    let Self {
      resolver,
      app,
      peer,
      header_timeout,
      handoff,
    } = self;
    let resolve = resolver.resolve_request();
    let resolved = match header_timeout {
      Some(timeout) => tokio::time::timeout(timeout, resolve).await.ok(),
      None => Some(resolve.await),
    };
    // A late, malformed or reset request: dropping the stream ends it (never `stop_sending`,
    // which panics in h3-quinn while a read is in flight, F13).
    let Some(Ok((head, stream))) = resolved else {
      return;
    };
    #[cfg(feature = "webtransport")]
    if let Some(handoff) = handoff
      && crate::server::webtransport::is_connect(&head)
    {
      return crate::server::webtransport::serve_connect(head, stream, app, peer, handoff).await;
    }
    #[cfg(not(feature = "webtransport"))]
    let _ = handoff;
    serve_request(head, stream, app, peer).await;
  }
}

/// Serves one resolved request through `app`: the response, then the rest of an unread body.
pub(crate) async fn serve_request(
  head: axum::http::Request<()>,
  stream: FullStream,
  app: Router,
  peer: SocketAddr,
) {
  let (send, recv) = stream.split();
  let head_only = head.method() == Method::HEAD;
  let body = Arc::new(Mutex::new(BodyState {
    stream: recv,
    data_done: false,
    done: false,
  }));
  let mut request = head.map(|()| {
    Body::new(H3Body {
      state: Arc::clone(&body),
    })
  });
  request.extensions_mut().insert(ConnectInfo(peer));
  let mut app = app;
  let Ok(response) = app.call(request).await;
  let _ = respond(send, response, head_only).await;
  finish_request_body(body).await;
}

/// Most a client may still send of an unread request body once the response is out.
const DRAIN_LIMIT: usize = 64 * 1024;

/// How long the rest of an unread request body is waited for.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// After the response: when the handler left the request body unread (a `GET` whose `FIN` is
/// still in flight, a handler that answered early), read the rest — at most [`DRAIN_LIMIT`]
/// bytes within [`DRAIN_TIMEOUT`] — so the client can finish its request stream instead of
/// seeing it stopped. The stream is then dropped, never `stop_sending`'d (F13).
async fn finish_request_body(body: Arc<Mutex<BodyState>>) {
  // Still held (a streaming response kept the body): leave it to its holder.
  let Ok(body) = Arc::try_unwrap(body) else {
    return;
  };
  let mut body = body.into_inner().unwrap_or_else(PoisonError::into_inner);
  if body.done {
    return;
  }
  let drain = async {
    let mut read = 0usize;
    while let Ok(Some(mut chunk)) = body.stream.recv_data().await {
      read += chunk.remaining();
      chunk.advance(chunk.remaining());
      if read > DRAIN_LIMIT {
        break;
      }
    }
  };
  let _ = tokio::time::timeout(DRAIN_TIMEOUT, drain).await;
}

/// Headers that are connection-specific in HTTP/1.1 and must not appear in HTTP/3 (RFC 9114
/// §4.2).
const CONNECTION_SPECIFIC: [header::HeaderName; 5] = [
  header::CONNECTION,
  header::TRANSFER_ENCODING,
  header::UPGRADE,
  header::HeaderName::from_static("keep-alive"),
  header::HeaderName::from_static("proxy-connection"),
];

/// Sends `response` on `send`: the head, the body frames (none for `HEAD`), trailers, finish.
/// A body that fails mid-way resets the stream (`H3_INTERNAL_ERROR`).
pub(crate) async fn respond(
  mut send: SendHalf,
  response: Response<Body>,
  head_only: bool,
) -> Result<(), StreamError> {
  let (mut parts, body) = response.into_parts();
  for name in &CONNECTION_SPECIFIC {
    parts.headers.remove(name);
  }
  send.send_response(Response::from_parts(parts, ())).await?;
  if !head_only {
    let mut body = std::pin::pin!(body);
    while let Some(frame) = body.frame().await {
      let Ok(frame) = frame else {
        send.stop_stream(Code::H3_INTERNAL_ERROR);
        return Ok(());
      };
      match frame.into_data() {
        Ok(data) if data.is_empty() => {}
        Ok(data) => send.send_data(data).await?,
        Err(frame) => {
          if let Ok(trailers) = frame.into_trailers() {
            send.send_trailers(trailers).await?;
            return Ok(());
          }
        }
      }
    }
  }
  send.finish().await
}

/// The receiving half of a request stream, shared by the body and its request task.
struct BodyState {
  stream: RecvHalf,
  data_done: bool,
  done: bool,
}

/// A request body read from the h3 stream: data frames, then trailers.
struct H3Body {
  state: Arc<Mutex<BodyState>>,
}

impl http_body::Body for H3Body {
  type Data = Bytes;
  type Error = StreamError;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Bytes>, StreamError>>> {
    let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
    let this = &mut *guard;
    if this.done {
      return Poll::Ready(None);
    }
    if !this.data_done {
      match ready!(this.stream.poll_recv_data(cx)) {
        Ok(Some(mut buf)) => {
          let data = buf.copy_to_bytes(buf.remaining());
          return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        Ok(None) => this.data_done = true,
        Err(e) => {
          this.done = true;
          return Poll::Ready(Some(Err(e)));
        }
      }
    }
    // `done` only once the trailers (or their absence) are known: a pending poll comes back.
    let trailers = ready!(this.stream.poll_recv_trailers(cx));
    this.done = true;
    match trailers {
      Ok(Some(trailers)) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
      Ok(None) => Poll::Ready(None),
      Err(e) => Poll::Ready(Some(Err(e))),
    }
  }
}

/// The HTTP/3 side of a host entry point: its own endpoint on the TCP port number.
#[derive(Debug)]
pub(crate) struct HostHttp3 {
  endpoint: QuicEndpoint,
}

impl HostHttp3 {
  /// Binds UDP on `tcp`'s address when `config` turns HTTP/3 on and `transport` is TLS with an
  /// HTTP/3 identity (certificate files); `None` otherwise (provided TLS settings are logged:
  /// TCP only).
  pub(crate) fn bind(
    config: &HostConfig,
    transport: &HostTransport,
    tcp: SocketAddr,
    limits: &ConnectionLimits,
  ) -> std::io::Result<Option<Self>> {
    if !config.http3 || !transport.is_tls() {
      return Ok(None);
    }
    let Some(identity) = transport.http3_identity() else {
      tracing::warn!(
        target: "id_effect_axum::posture",
        "HTTP/3 is on but the TLS settings were provided (HostTls::Provided), which carry no \
         certificate identity for QUIC; serving TCP only, without Alt-Svc (use certificate \
         files, or serve_h3 on an endpoint of your own)"
      );
      return Ok(None);
    };
    let config = QuicEndpointConfig::new(identity.clone()).with_limits(quic_limits(limits));
    let endpoint = QuicEndpoint::bind(tcp, config).map_err(io_error)?;
    Ok(Some(Self { endpoint }))
  }

  /// The UDP port (the TCP one).
  pub(crate) fn port(&self) -> u16 {
    self.endpoint.local_addr().port()
  }

  /// [`serve_h3`] with the default [`Http3Options`], then closes the endpoint.
  pub(crate) async fn serve<F>(
    self,
    app: Router,
    limits: ConnectionLimits,
    shutdown: F,
  ) -> std::io::Result<()>
  where
    F: Future<Output = ()> + Send + 'static,
  {
    let served = serve_h3(
      &self.endpoint,
      app,
      limits,
      Http3Options::default(),
      shutdown,
    )
    .await;
    self.endpoint.close();
    served
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn quic_limits_follow_the_connection_limits_under_the_ceiling() {
    let quic = quic_limits(&ConnectionLimits::default());
    assert_eq!(quic, QuicLimits::default());
    let tight = ConnectionLimits::default()
      .max_concurrent_streams(8)
      .tls_handshake_timeout(Duration::from_secs(2));
    let quic = quic_limits(&tight);
    assert_eq!(quic.max_concurrent_bidi_streams, 8);
    assert_eq!(quic.handshake_timeout, Duration::from_secs(2));
    // Above the endpoint ceiling, or unlimited on TCP: still the finite ceiling on QUIC.
    let loose = ConnectionLimits::default()
      .max_concurrent_streams(1_000)
      .tls_handshake_timeout(Duration::from_secs(60))
      .dangerous_unlimited_concurrent_streams()
      .dangerous_no_tls_handshake_timeout();
    assert_eq!(quic_limits(&loose), QuicLimits::default());
    let wide = ConnectionLimits::default().max_concurrent_streams(1_000);
    assert_eq!(quic_limits(&wide).max_concurrent_bidi_streams, 100);
  }

  #[test]
  fn zero_rtt_is_off_and_the_opt_out_admits_safe_methods_only() {
    let options = Http3Options::new();
    assert!(!options.zero_rtt_for_queries());
    assert!(!options.admits_early(&Method::GET));
    let options = options.dangerous_allow_zero_rtt_for_queries();
    assert!(options.zero_rtt_for_queries());
    for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
      assert!(options.admits_early(&method), "{method}");
    }
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
      assert!(!options.admits_early(&method), "{method}");
    }
    assert!(
      id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == DANGEROUS_ALLOW_ZERO_RTT_FOR_QUERIES)
    );
  }
}
