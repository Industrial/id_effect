//! WebSockets over TLS HTTP/2 (RFC 8441 extended `CONNECT`, F15, r13-f-ext-connect) on real
//! sockets: the same typed route serves the HTTP/2 `CONNECT :protocol websocket` and the
//! HTTP/1.1 upgrade, with the same guards, Origin allowlist, socket caps and limits (ADR 0013
//! R9, R4), and the HTTP/2 concurrent-stream cap bounding the sockets of one connection.
#![cfg(all(feature = "tls", feature = "ws"))]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, Request, StatusCode, Version, header};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use http_body_util::Empty;
use hyper_util::rt::{TokioExecutor, TokioIo};
use id_effect::{Effect, Never, succeed};
use id_effect_axum::realtime::ws::{self, WsConfig, WsSocket};
use id_effect_axum::route::{GuardFuture, Input, Route, RouteGuard, Router};
use id_effect_axum::server::serve_tls_listener;
use id_effect_axum::server::tls::ServerTls;
use id_effect_axum::{CapOverrideLayer, ConnectionLimits, RequestCaps};
use id_effect_platform::tls::{Identity, NodeCertificate, provider};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Role;

const WAIT: Duration = Duration::from_secs(10);
const ALLOWED: &str = "https://app.example.com";

/// Any client connection a WebSocket runs on: a TLS stream (HTTP/1.1) or an HTTP/2 stream.
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The client end of a socket. Once muted, what the client writes is dropped: a client whose
/// pongs (and everything else) never reach the server, though it keeps reading.
struct Client {
  io: Box<dyn Io>,
  mute: AtomicBool,
}

impl Client {
  fn new(io: impl Io + 'static) -> Self {
    Self {
      io: Box::new(io),
      mute: AtomicBool::new(false),
    }
  }

  fn muted(&self) -> bool {
    self.mute.load(Ordering::SeqCst)
  }
}

impl AsyncRead for Client {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<std::io::Result<()>> {
    Pin::new(&mut self.io).poll_read(cx, buf)
  }
}

impl AsyncWrite for Client {
  fn poll_write(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &[u8],
  ) -> Poll<std::io::Result<usize>> {
    if self.muted() {
      return Poll::Ready(Ok(buf.len()));
    }
    Pin::new(&mut self.io).poll_write(cx, buf)
  }

  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    if self.muted() {
      return Poll::Ready(Ok(()));
    }
    Pin::new(&mut self.io).poll_flush(cx)
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
    if self.muted() {
      return Poll::Ready(Ok(()));
    }
    Pin::new(&mut self.io).poll_shutdown(cx)
  }
}

type Ws = WebSocketStream<Client>;
type H2 = hyper::client::conn::http2::SendRequest<Empty<Bytes>>;

/// A TLS host serving `app` with `limits` on an ephemeral port, and the root that trusts it.
struct Server {
  addr: SocketAddr,
  roots: RootCertStore,
  _stop: tokio::sync::oneshot::Sender<()>,
}

async fn serve(app: axum::Router, limits: ConnectionLimits) -> Server {
  let cert = NodeCertificate::self_signed("localhost").unwrap();
  let identity = Identity::from_certificate(&cert).unwrap();
  let tls = ServerTls::from_identity(&identity).unwrap();
  let mut roots = RootCertStore::empty();
  roots
    .add(CertificateDer::from(cert.chain()[0].to_vec()))
    .unwrap();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
  tokio::spawn(serve_tls_listener(listener, app, limits, tls, async move {
    let _ = stopped.await;
  }));
  Server {
    addr,
    roots,
    _stop: stop,
  }
}

impl Server {
  async fn tls(&self, alpn: &[u8]) -> tokio_rustls::client::TlsStream<TcpStream> {
    let mut config = ClientConfig::builder_with_provider(provider())
      .with_safe_default_protocol_versions()
      .unwrap()
      .with_root_certificates(self.roots.clone())
      .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    let tcp = TcpStream::connect(self.addr).await.unwrap();
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
      .connect(ServerName::try_from("localhost").unwrap(), tcp)
      .await
      .unwrap();
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(alpn));
    stream
  }

  /// An HTTP/2 connection whose client has the server's SETTINGS (one round trip done: a plain
  /// `GET` on the WebSocket route, refused with 426 and no `Upgrade` header on HTTP/2).
  async fn h2(&self) -> H2 {
    let stream = self.tls(b"h2").await;
    let (mut send, conn) =
      hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
      let _ = conn.await;
    });
    let warm_up = Request::get("https://localhost/echo")
      .body(Empty::new())
      .unwrap();
    let res = send.send_request(warm_up).await.unwrap();
    assert_eq!(res.version(), Version::HTTP_2);
    assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
    assert!(res.headers().get(header::UPGRADE).is_none());
    send
  }
}

/// The opening request's extra headers.
#[derive(Clone, Copy, Default)]
struct Opening {
  origin: Option<&'static str>,
  protocols: Option<&'static str>,
  key: Option<&'static str>,
  user: Option<&'static str>,
}

impl Opening {
  fn apply(self, headers: &mut axum::http::HeaderMap) {
    for (name, value) in [
      ("origin", self.origin),
      ("sec-websocket-protocol", self.protocols),
      ("x-key", self.key),
      ("x-user", self.user),
    ] {
      if let Some(value) = value {
        headers.insert(name, HeaderValue::from_static(value));
      }
    }
  }
}

/// Opens `path` with an extended `CONNECT` on the HTTP/2 connection `send`: the socket and the
/// selected subprotocol, or the refusal's status.
async fn open_h2(
  send: &mut H2,
  path: &str,
  opening: Opening,
) -> Result<(Ws, Option<String>), StatusCode> {
  open_h2_as(send, path, opening, "websocket").await
}

async fn open_h2_as(
  send: &mut H2,
  path: &str,
  opening: Opening,
  protocol: &'static str,
) -> Result<(Ws, Option<String>), StatusCode> {
  let mut req = Request::builder()
    .method(Method::CONNECT)
    .uri(format!("https://localhost{path}"))
    .header("sec-websocket-version", "13")
    .body(Empty::new())
    .unwrap();
  opening.apply(req.headers_mut());
  req
    .extensions_mut()
    .insert(hyper::ext::Protocol::from_static(protocol));
  let res = send.send_request(req).await.unwrap();
  assert_eq!(res.version(), Version::HTTP_2);
  if res.status() != StatusCode::OK {
    return Err(res.status());
  }
  // RFC 8441: no RFC 6455 handshake headers on HTTP/2.
  assert!(res.headers().get("sec-websocket-accept").is_none());
  let selected = res
    .headers()
    .get("sec-websocket-protocol")
    .map(|v| v.to_str().unwrap().to_owned());
  let upgraded = hyper::upgrade::on(res).await.unwrap();
  let io = Client::new(TokioIo::new(upgraded));
  Ok((
    WebSocketStream::from_raw_socket(io, Role::Client, None).await,
    selected,
  ))
}

/// Opens `path` with the HTTP/1.1 upgrade over its own TLS connection (ALPN `http/1.1`).
async fn open_h1(
  server: &Server,
  path: &str,
  opening: Opening,
) -> Result<(Ws, Option<String>), StatusCode> {
  let stream = Client::new(server.tls(b"http/1.1").await);
  let mut req = format!("wss://localhost{path}")
    .into_client_request()
    .unwrap();
  opening.apply(req.headers_mut());
  match tokio_tungstenite::client_async(req, stream).await {
    Ok((ws, res)) => {
      assert_eq!(res.status(), StatusCode::SWITCHING_PROTOCOLS);
      let selected = res
        .headers()
        .get("sec-websocket-protocol")
        .map(|v| v.to_str().unwrap().to_owned());
      Ok((ws, selected))
    }
    Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status()),
    Err(e) => panic!("unexpected handshake error: {e}"),
  }
}

/// How a test opens sockets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
  /// HTTP/1.1 upgrade over TLS, one connection per socket.
  H1,
  /// HTTP/2 extended `CONNECT`, one connection per socket.
  H2,
}

/// A socket and, on HTTP/2, the handle that keeps its connection open.
struct Socket {
  ws: Ws,
  _conn: Option<H2>,
}

impl std::ops::Deref for Socket {
  type Target = Ws;

  fn deref(&self) -> &Ws {
    &self.ws
  }
}

impl std::ops::DerefMut for Socket {
  fn deref_mut(&mut self) -> &mut Ws {
    &mut self.ws
  }
}

impl Transport {
  async fn open(self, server: &Server, path: &str, opening: Opening) -> Result<Socket, StatusCode> {
    match self {
      Transport::H1 => open_h1(server, path, opening)
        .await
        .map(|(ws, _)| Socket { ws, _conn: None }),
      Transport::H2 => {
        let mut send = server.h2().await;
        let (ws, _) = open_h2(&mut send, path, opening).await?;
        Ok(Socket {
          ws,
          _conn: Some(send),
        })
      }
    }
  }
}

fn echo(_: &mut (), _: RequestCaps, socket: WsSocket) -> Effect<(), Never, ()> {
  let (incoming, out) = socket.into_parts::<Never, ()>();
  incoming.run_for_each_effect(move |msg| out.send(msg).catch(|_| succeed(())))
}

/// Admits requests with `x-key: open-sesame`.
struct KeyGuard;

impl RouteGuard for KeyGuard {
  fn policy(&self) -> &str {
    "key"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let ok = parts
      .headers
      .get("x-key")
      .is_some_and(|v| v == "open-sesame");
    Box::pin(async move {
      if ok {
        Ok(())
      } else {
        Err(StatusCode::UNAUTHORIZED.into_response())
      }
    })
  }
}

struct Principal(String);

/// Sends `"<principal> <subprotocol> <client ip>"`, then returns (close 1000).
fn whoami(_: &mut (), caps: RequestCaps, socket: WsSocket) -> Effect<(), Never, ()> {
  let name = caps
    .get::<Principal>()
    .map_or_else(|| "anonymous".to_owned(), |p| p.0.clone());
  let protocol = socket.protocol().unwrap_or("none").to_owned();
  let ip = socket
    .client_ip()
    .map(|ip| ip.to_string())
    .unwrap_or_default();
  socket
    .sender()
    .send::<()>(format!("{name} {protocol} {ip}"))
    .catch(|_| succeed(()))
}

fn hello(_: &mut (), _: Input) -> Effect<&'static str, Never, ()> {
  succeed("hello")
}

/// `/echo` (public, `config`), `/me` (guarded, subprotocols `relay.v1`, `relay.v0`) and a plain
/// `GET /hello`.
fn app(config: WsConfig) -> axum::Router {
  let me = WsConfig::default()
    .allowed_origins([ALLOWED])
    .unwrap()
    .protocols(["relay.v1", "relay.v0"]);
  Router::new()
    .route(ws::endpoint("/echo", config, echo).public())
    .route(ws::endpoint("/me", me, whoami).guard(KeyGuard))
    .route(Route::get("/hello").public().handler(hello))
    .with_state(())
    .into_axum()
    .layer(CapOverrideLayer::new(|parts: &Parts| {
      let user = parts.headers.get("x-user")?.to_str().ok()?;
      Some(Principal(user.to_owned()))
    }))
}

fn allowed() -> WsConfig {
  WsConfig::default().allowed_origins([ALLOWED]).unwrap()
}

async fn round_trip(ws: &mut Ws, text: &str) {
  ws.send(Message::text(text)).await.unwrap();
  let echoed = tokio::time::timeout(WAIT, ws.next()).await.unwrap();
  assert_eq!(echoed.unwrap().unwrap(), Message::text(text));
}

/// Reads until the server's close frame (answering pings on the way): its code and reason.
async fn close_of(ws: &mut Ws) -> Option<(u16, String)> {
  tokio::time::timeout(WAIT, async {
    while let Some(msg) = ws.next().await {
      match msg {
        Ok(Message::Close(Some(frame))) => {
          return Some((u16::from(frame.code), frame.reason.to_string()));
        }
        Ok(Message::Close(None)) | Err(_) => return None,
        Ok(_) => {}
      }
    }
    None
  })
  .await
  .expect("the server closes the socket in time")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_over_h2_round_trip() {
  let server = serve(app(allowed()), ConnectionLimits::default()).await;
  let mut send = server.h2().await;
  let origin = Opening {
    origin: Some(ALLOWED),
    ..Opening::default()
  };
  // Two sockets multiplexed on one HTTP/2 connection.
  let (mut a, selected) = open_h2(&mut send, "/echo", origin).await.unwrap();
  assert_eq!(selected, None);
  let (mut b, _) = open_h2(&mut send, "/echo", origin).await.unwrap();
  round_trip(&mut a, "over h2").await;
  round_trip(&mut b, "beside it").await;
  a.send(Message::binary(vec![1_u8, 2, 3])).await.unwrap();
  assert_eq!(
    a.next().await.unwrap().unwrap(),
    Message::binary(vec![1_u8, 2, 3])
  );
  a.send(Message::Close(None)).await.unwrap();
  assert!(matches!(a.next().await, Some(Ok(Message::Close(_)))));
  // The connection and its other socket live on.
  round_trip(&mut b, "still here").await;

  // A guarded route: the guard, the request capabilities, the subprotocol and the client IP
  // reach the handler as on HTTP/1.1.
  assert_eq!(
    open_h2(&mut send, "/me", origin).await.err(),
    Some(StatusCode::UNAUTHORIZED)
  );
  let opening = Opening {
    origin: Some(ALLOWED),
    protocols: Some("other, relay.v0, relay.v1"),
    key: Some("open-sesame"),
    user: Some("ada"),
  };
  let (mut me, selected) = open_h2(&mut send, "/me", opening).await.unwrap();
  assert_eq!(selected.as_deref(), Some("relay.v1"));
  assert_eq!(
    me.next().await.unwrap().unwrap(),
    Message::text("ada relay.v1 127.0.0.1")
  );
  assert_eq!(close_of(&mut me).await, Some((1000, String::new())));

  // An extended CONNECT for another protocol is not a WebSocket opening.
  assert_eq!(
    open_h2_as(&mut send, "/echo", origin, "webtransport")
      .await
      .err(),
    Some(StatusCode::UPGRADE_REQUIRED)
  );
  // A route that serves no WebSocket does not take a CONNECT: a plain `GET` route answers 405,
  // unregistered paths stay 404.
  assert_eq!(
    open_h2(&mut send, "/hello", origin).await.err(),
    Some(StatusCode::METHOD_NOT_ALLOWED)
  );
  assert_eq!(
    open_h2(&mut send, "/nowhere", origin).await.err(),
    Some(StatusCode::NOT_FOUND)
  );
  let res = send
    .send_request(
      Request::get("https://localhost/hello")
        .body(Empty::new())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h1_websocket_unchanged() {
  let server = serve(app(allowed()), ConnectionLimits::default()).await;
  let origin = Opening {
    origin: Some(ALLOWED),
    ..Opening::default()
  };
  // HTTP/1.1 over TLS: the `101` upgrade with its accept key (checked by the client).
  let (mut ws, selected) = open_h1(&server, "/echo", origin).await.unwrap();
  assert_eq!(selected, None);
  round_trip(&mut ws, "over http/1.1").await;
  let opening = Opening {
    origin: Some(ALLOWED),
    protocols: Some("relay.v0"),
    key: Some("open-sesame"),
    user: Some("bo"),
  };
  let (mut me, selected) = open_h1(&server, "/me", opening).await.unwrap();
  assert_eq!(selected.as_deref(), Some("relay.v0"));
  assert_eq!(
    me.next().await.unwrap().unwrap(),
    Message::text("bo relay.v0 127.0.0.1")
  );
  assert_eq!(
    open_h1(&server, "/me", origin).await.err(),
    Some(StatusCode::UNAUTHORIZED)
  );

  // A plain HTTP/1.1 GET is refused with 426 naming the upgrade, as before.
  let stream = server.tls(b"http/1.1").await;
  let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
    .await
    .unwrap();
  tokio::spawn(async move {
    let _ = conn.await;
  });
  let res = send
    .send_request(
      Request::get("/echo")
        .header(header::HOST, "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
  assert_eq!(res.headers()[header::UPGRADE], "websocket");

  // Plain TCP (HTTP/1.1 only) serves the same route.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (_stop, stopped) = tokio::sync::oneshot::channel::<()>();
  tokio::spawn(id_effect_axum::serve_listener(
    listener,
    app(allowed()),
    ConnectionLimits::default(),
    async move {
      let _ = stopped.await;
    },
  ));
  let mut req = format!("ws://{addr}/echo").into_client_request().unwrap();
  req
    .headers_mut()
    .insert("origin", HeaderValue::from_static(ALLOWED));
  let (mut plain, res) = tokio_tungstenite::connect_async(req).await.unwrap();
  assert_eq!(res.status(), StatusCode::SWITCHING_PROTOCOLS);
  plain.send(Message::text("plain")).await.unwrap();
  assert_eq!(plain.next().await.unwrap().unwrap(), Message::text("plain"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r9_h2_extended_connect_foreign_origin_refused() {
  let server = serve(app(allowed()), ConnectionLimits::default()).await;
  let foreign = Opening {
    origin: Some("https://evil.example"),
    ..Opening::default()
  };
  // Refused before the `200`, on both transports, and on the guarded route even with its key.
  for transport in [Transport::H2, Transport::H1] {
    assert_eq!(
      transport.open(&server, "/echo", foreign).await.err(),
      Some(StatusCode::FORBIDDEN),
      "{transport:?}"
    );
    let keyed = Opening {
      key: Some("open-sesame"),
      ..foreign
    };
    assert_eq!(
      transport.open(&server, "/me", keyed).await.err(),
      Some(StatusCode::FORBIDDEN),
      "{transport:?}"
    );
  }
  // The default allowlist refuses every browser origin, the allowed one included.
  let closed = serve(app(WsConfig::default()), ConnectionLimits::default()).await;
  let mut send = closed.h2().await;
  for origin in [ALLOWED, "https://evil.example", "null"] {
    let opening = Opening {
      origin: Some(origin),
      ..Opening::default()
    };
    assert_eq!(
      open_h2(&mut send, "/echo", opening).await.err(),
      Some(StatusCode::FORBIDDEN),
      "{origin}"
    );
  }
  // The connection survives the refusals; the allowed origin opens on the first server.
  let mut send = server.h2().await;
  let allowed = Opening {
    origin: Some(ALLOWED),
    ..Opening::default()
  };
  let (mut ws, _) = open_h2(&mut send, "/echo", allowed).await.unwrap();
  round_trip(&mut ws, "allowed").await;
}

/// What each 0.12 limit does to a socket opened over `transport`.
async fn limit_outcomes(transport: Transport) -> Vec<Option<(u16, String)>> {
  let origin = Opening {
    origin: Some(ALLOWED),
    ..Opening::default()
  };
  let mut out = Vec::new();

  // Frame and message size: 1 KiB, close 1009.
  let server = serve(
    app(allowed().max_message_bytes(1024, 1024)),
    ConnectionLimits::default(),
  )
  .await;
  let mut ws = transport.open(&server, "/echo", origin).await.unwrap();
  round_trip(&mut ws, &"x".repeat(1024)).await;
  ws.send(Message::binary(vec![0_u8; 1025])).await.unwrap();
  out.push(close_of(&mut ws).await);

  // Message rate: 2 per second, close 1008.
  let server = serve(
    app(allowed().messages_per_second(2)),
    ConnectionLimits::default(),
  )
  .await;
  let mut ws = transport.open(&server, "/echo", origin).await.unwrap();
  for n in 0..6 {
    // The socket may already be closing.
    let _ = ws.send(Message::text(n.to_string())).await;
  }
  out.push(close_of(&mut ws).await);

  // Control frames: their own cap (4 per second here), close 1008.
  let server = serve(
    app(
      allowed()
        .messages_per_second(100)
        .control_messages_per_second(4),
    ),
    ConnectionLimits::default(),
  )
  .await;
  let mut ws = transport.open(&server, "/echo", origin).await.unwrap();
  for _ in 0..12 {
    let _ = ws.send(Message::Ping(Bytes::new())).await;
  }
  out.push(close_of(&mut ws).await);

  // Idle timeout, close 1001.
  let server = serve(
    app(
      allowed()
        .idle_timeout(Duration::from_millis(300))
        .heartbeat(Duration::from_secs(3600), 2),
    ),
    ConnectionLimits::default(),
  )
  .await;
  let mut ws = transport.open(&server, "/echo", origin).await.unwrap();
  out.push(close_of(&mut ws).await);

  // Heartbeat: a client that never answers is closed with 1001 after the missed pongs.
  let server = serve(
    app(allowed().heartbeat(Duration::from_millis(100), 1)),
    ConnectionLimits::default(),
  )
  .await;
  let mut ws = transport.open(&server, "/echo", origin).await.unwrap();
  // The client keeps reading, but its pongs never reach the server.
  ws.get_ref().mute.store(true, Ordering::SeqCst);
  out.push(close_of(&mut ws).await);

  // Sockets per client IP: the second one is refused with 429 before the opening completes.
  let server = serve(
    app(allowed().max_sockets(100, 1)),
    ConnectionLimits::default(),
  )
  .await;
  let mut first = transport.open(&server, "/echo", origin).await.unwrap();
  let second = transport.open(&server, "/echo", origin).await.err();
  out.push(second.map(|status| (status.as_u16(), "refused".to_owned())));
  round_trip(&mut first, "first").await;
  out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h2_websocket_limits_match_h1() {
  let h2 = limit_outcomes(Transport::H2).await;
  let h1 = limit_outcomes(Transport::H1).await;
  let expected = [
    (1009, "message too big"),
    (1008, "message rate exceeded"),
    (1008, "control frame rate exceeded"),
    (1001, "idle timeout"),
    (1001, "heartbeat timeout"),
    (429, "refused"),
  ]
  .map(|(code, reason)| Some((code, reason.to_owned())));
  assert_eq!(h2, expected);
  assert_eq!(h1, h2, "HTTP/2 sockets get exactly the HTTP/1.1 limits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h2_websockets_bounded_by_concurrent_stream_cap() {
  let server = serve(
    app(allowed()),
    ConnectionLimits::new().max_concurrent_streams(2),
  )
  .await;
  let origin = Opening {
    origin: Some(ALLOWED),
    ..Opening::default()
  };
  let mut send = server.h2().await;
  let (mut a, _) = open_h2(&mut send, "/echo", origin).await.unwrap();
  let (mut b, _) = open_h2(&mut send, "/echo", origin).await.unwrap();
  round_trip(&mut a, "a").await;
  round_trip(&mut b, "b").await;
  // A third socket waits for a stream: both are held by open sockets.
  let mut third_send = send.clone();
  let third = tokio::spawn(async move { open_h2(&mut third_send, "/echo", origin).await });
  tokio::time::sleep(Duration::from_millis(300)).await;
  assert!(!third.is_finished(), "a third stream opened past the cap");
  // Closing one socket frees its stream.
  a.send(Message::Close(None)).await.unwrap();
  assert!(matches!(a.next().await, Some(Ok(Message::Close(_)))));
  drop(a);
  let (mut c, _) = tokio::time::timeout(WAIT, third)
    .await
    .expect("the third socket opens once a stream is free")
    .unwrap()
    .unwrap();
  round_trip(&mut c, "c").await;
  round_trip(&mut b, "b again").await;
}
