//! WebSocket endpoints (feature `ws`): sessions on the test clock over an in-memory pipe, and
//! the HTTP upgrade (Origin, caps, guards, subprotocols) on a real listener.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{HeaderValue, Request, StatusCode};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use id_effect::{
  Effect, HNil, Never, Stream, TestExecutor, fail, from_async, sleep, succeed, sync,
};
use id_effect_axum::realtime::origin::ANY_ORIGIN_OPT_OUT;
use id_effect_axum::realtime::ws::{self, WsClose, WsConfig, WsMessage, WsSocket, serve_io};
use id_effect_axum::route::{GuardFuture, RouteGuard, Router};
use id_effect_axum::{CapOverrideLayer, ConnectionLimits, RequestCaps, serve_listener};
use tokio::io::DuplexStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Role;
use tower::ServiceExt;

type Client = Arc<AsyncMutex<WebSocketStream<DuplexStream>>>;
type Slot<T> = Arc<Mutex<Option<T>>>;

/// Runs `fut` as a fiber on `exec` until it stalls; its output lands in the slot.
fn drive<T, F>(exec: &TestExecutor, fut: F) -> Slot<T>
where
  T: Send + 'static,
  F: Future<Output = T> + Send + 'static,
{
  let slot: Slot<T> = Arc::new(Mutex::new(None));
  let out = Arc::clone(&slot);
  let eff: Effect<(), Never, HNil> = from_async(move || async move {
    let v = fut.await;
    *out.lock().unwrap() = Some(v);
    Ok(())
  });
  drop(eff.fork_root(Arc::new(exec.clone())));
  exec.run_until_stalled();
  slot
}

fn take<T>(slot: &Slot<T>) -> T {
  slot.lock().unwrap().take().expect("the step completed")
}

/// A server session on `exec` running `handler`, and the client end of the pipe.
fn session<E, F>(exec: &TestExecutor, config: WsConfig, handler: F) -> (Slot<WsClose>, Client)
where
  E: Send + 'static,
  F: FnOnce(WsSocket) -> Effect<(), E, HNil> + Send + 'static,
{
  let (server_io, client_io) = tokio::io::duplex(1 << 20);
  let closed: Slot<WsClose> = Arc::new(Mutex::new(None));
  let out = Arc::clone(&closed);
  let server = serve_io(server_io, &config, handler).map(move |close| {
    *out.lock().unwrap() = Some(close);
  });
  drop(server.fork_root(Arc::new(exec.clone())));
  let client = take(&drive(exec, async move {
    WebSocketStream::from_raw_socket(client_io, Role::Client, None).await
  }));
  (closed, Arc::new(AsyncMutex::new(client)))
}

fn echo(socket: WsSocket) -> Effect<(), Never, HNil> {
  let (incoming, out) = socket.into_parts::<Never, HNil>();
  incoming.run_for_each_effect(move |msg| out.send(msg).catch(|_| succeed(())))
}

/// Reads the next message (`None` at the end or on an error).
fn recv(exec: &TestExecutor, client: &Client) -> Option<Message> {
  let client = Arc::clone(client);
  take(&drive(exec, async move {
    client.lock().await.next().await.and_then(Result::ok)
  }))
}

fn send(exec: &TestExecutor, client: &Client, msg: Message) {
  let client = Arc::clone(client);
  take(&drive(exec, async move {
    client.lock().await.send(msg).await.unwrap();
  }));
}

fn close_code(msg: Option<Message>) -> Option<(u16, String)> {
  match msg {
    Some(Message::Close(Some(frame))) => Some((u16::from(frame.code), frame.reason.to_string())),
    other => panic!("expected a close frame, got {other:?}"),
  }
}

fn finalizer(flag: &Arc<AtomicBool>) -> Effect<(), Never, HNil> {
  let flag = Arc::clone(flag);
  sync(move || flag.store(true, Ordering::SeqCst))
}

fn advance(exec: &TestExecutor, d: Duration) {
  exec.advance(d);
  exec.run_until_stalled();
}

#[test]
fn ws_round_trip_under_test_clock() {
  let exec = TestExecutor::new();
  let (closed, client) = session(&exec, WsConfig::default(), echo);
  send(&exec, &client, Message::text("hi"));
  assert_eq!(recv(&exec, &client), Some(Message::text("hi")));
  send(&exec, &client, Message::binary(vec![1_u8, 2, 3]));
  assert_eq!(
    recv(&exec, &client),
    Some(Message::binary(vec![1_u8, 2, 3]))
  );
  // Virtual time: a ping every 20 s; the client answers (tungstenite queues the pong).
  for _ in 0..4 {
    advance(&exec, Duration::from_secs(20));
    assert!(matches!(recv(&exec, &client), Some(Message::Ping(_))));
    let c = Arc::clone(&client);
    take(&drive(&exec, async move {
      c.lock().await.flush().await.unwrap();
    }));
  }
  assert!(
    closed.lock().unwrap().is_none(),
    "answered pings keep the socket open"
  );
  send(&exec, &client, Message::Close(None));
  assert!(matches!(recv(&exec, &client), Some(Message::Close(_))));
  exec.run_until_stalled();
  assert_eq!(take(&closed), WsClose::ClientClosed(None));
}

#[test]
fn heartbeat_closes_dead_connection() {
  let exec = TestExecutor::new();
  let finalized = Arc::new(AtomicBool::new(false));
  let f = Arc::clone(&finalized);
  let (closed, client) = session(&exec, WsConfig::default(), move |_socket| {
    sleep::<Never, HNil>(Duration::from_secs(86_400)).ensuring(finalizer(&f))
  });
  // The client never reads, so it never answers: pings at 20 s and 40 s, closed at 60 s.
  advance(&exec, Duration::from_secs(20));
  advance(&exec, Duration::from_secs(20));
  assert!(
    closed.lock().unwrap().is_none(),
    "one missed pong is tolerated"
  );
  advance(&exec, Duration::from_secs(20));
  assert_eq!(take(&closed), WsClose::HeartbeatTimeout);
  assert!(
    finalized.load(Ordering::SeqCst),
    "the handler was interrupted"
  );
  // What the server wrote, raw (a client reading it would try to answer the pings on a closed
  // pipe): ping, ping, close 1001.
  let c = Arc::clone(&client);
  let wire = take(&drive(&exec, async move {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let _ = c.lock().await.get_mut().read_to_end(&mut buf).await;
    buf
  }));
  let mut close = vec![0x88, 2 + 17, 0x03, 0xE9];
  close.extend_from_slice(b"heartbeat timeout");
  assert_eq!(wire, [&[0x89, 0x00][..], &[0x89, 0x00], &close].concat());
}

#[test]
fn idle_timeout_closes_a_quiet_socket() {
  let exec = TestExecutor::new();
  let config = WsConfig::default()
    .idle_timeout(Duration::from_secs(30))
    .heartbeat(Duration::from_secs(3600), 2);
  let (closed, client) = session(&exec, config, echo);
  advance(&exec, Duration::from_secs(10));
  send(&exec, &client, Message::text("still here"));
  assert_eq!(recv(&exec, &client), Some(Message::text("still here")));
  advance(&exec, Duration::from_secs(29));
  assert!(
    closed.lock().unwrap().is_none(),
    "activity moved the deadline"
  );
  advance(&exec, Duration::from_secs(2));
  assert_eq!(take(&closed), WsClose::IdleTimeout);
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1001, "idle timeout".into()))
  );
}

#[test]
fn dropped_socket_interrupts_handler() {
  let exec = TestExecutor::new();
  let finalized = Arc::new(AtomicBool::new(false));
  let f = Arc::clone(&finalized);
  let (closed, client) = session(&exec, WsConfig::default(), move |_socket| {
    sleep::<Never, HNil>(Duration::from_secs(86_400)).ensuring(finalizer(&f))
  });
  assert!(!finalized.load(Ordering::SeqCst));
  drop(client);
  exec.run_until_stalled();
  assert_eq!(take(&closed), WsClose::ClientGone);
  assert!(
    finalized.load(Ordering::SeqCst),
    "the handler's finalizer ran"
  );
}

#[test]
fn r4_ws_frame_over_max_closes_1009() {
  let exec = TestExecutor::new();
  let (closed, client) = session(&exec, WsConfig::default(), echo);
  send(&exec, &client, Message::binary(vec![7_u8; 64 * 1024]));
  assert_eq!(
    recv(&exec, &client),
    Some(Message::binary(vec![7_u8; 64 * 1024]))
  );
  send(&exec, &client, Message::binary(vec![7_u8; 64 * 1024 + 1]));
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1009, "message too big".into()))
  );
  exec.run_until_stalled();
  assert_eq!(take(&closed), WsClose::MessageTooBig);
}

#[test]
fn r4_ws_message_rate_closes_1008() {
  let exec = TestExecutor::new();
  let config = WsConfig::default().messages_per_second(2);
  let (closed, client) = session(&exec, config, echo);
  send(&exec, &client, Message::text("0"));
  send(&exec, &client, Message::text("1"));
  advance(&exec, Duration::from_millis(500));
  send(&exec, &client, Message::text("2"));
  for n in 0..3 {
    assert_eq!(recv(&exec, &client), Some(Message::text(n.to_string())));
  }
  send(&exec, &client, Message::text("3"));
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1008, "message rate exceeded".into()))
  );
  exec.run_until_stalled();
  assert_eq!(take(&closed), WsClose::RateLimited);
}

#[test]
fn a_returning_handler_flushes_then_closes_1000_and_a_failing_one_1011() {
  let exec = TestExecutor::new();
  let (closed, client) = session(&exec, WsConfig::default(), |socket: WsSocket| {
    let out = socket.sender();
    Stream::<WsMessage, ws::WsClosed>::from_iterable(["a", "b", "c"].map(WsMessage::from))
      .run(out.sink())
  });
  for text in ["a", "b", "c"] {
    assert_eq!(recv(&exec, &client), Some(Message::text(text)));
  }
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1000, String::new()))
  );
  assert_eq!(take(&closed), WsClose::HandlerDone);

  let (closed, client) = session(&exec, WsConfig::default(), |socket: WsSocket| {
    socket
      .sender()
      .send::<HNil>("bye")
      .flat_map(|()| fail::<(), ws::WsClosed, HNil>(ws::WsClosed))
  });
  assert_eq!(recv(&exec, &client), Some(Message::text("bye")));
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1011, "internal error".into()))
  );
  assert_eq!(take(&closed), WsClose::HandlerFailed);
}

#[test]
fn backpressure_holds_reads_while_the_handler_is_slow() {
  let exec = TestExecutor::new();
  let seen = Arc::new(Mutex::new(Vec::new()));
  let s = Arc::clone(&seen);
  let config = WsConfig::default().buffers(1, 1);
  let (closed, client) = session(&exec, config, move |socket: WsSocket| {
    let (incoming, out) = socket.into_parts::<Never, HNil>();
    assert!(!out.is_closed());
    // One message per virtual second.
    incoming.run_for_each_effect(move |msg| {
      let s = Arc::clone(&s);
      sleep(Duration::from_secs(1)).map(move |()| s.lock().unwrap().push(msg))
    })
  });
  for n in 0..5 {
    send(&exec, &client, Message::text(n.to_string()));
  }
  assert!(seen.lock().unwrap().is_empty());
  for _ in 0..6 {
    advance(&exec, Duration::from_secs(1));
  }
  let got: Vec<String> = seen
    .lock()
    .unwrap()
    .iter()
    .map(|m: &WsMessage| m.as_text().unwrap().to_owned())
    .collect();
  assert_eq!(got, ["0", "1", "2", "3", "4"], "every message, in order");
  drop(client);
  exec.run_until_stalled();
  assert_eq!(take(&closed), WsClose::ClientGone);
}

/// A final, masked client frame with `opcode` and `payload` (< 126 bytes).
fn raw_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
  let mask = [1_u8, 2, 3, 4];
  let mut out = vec![0x80 | opcode, 0x80 | u8::try_from(payload.len()).unwrap()];
  out.extend_from_slice(&mask);
  out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
  out
}

fn write_raw(exec: &TestExecutor, client: &Client, bytes: Vec<u8>) {
  let c = Arc::clone(client);
  take(&drive(exec, async move {
    use tokio::io::AsyncWriteExt;
    c.lock().await.get_mut().write_all(&bytes).await.unwrap();
  }));
}

#[test]
fn protocol_errors_close_1002_and_bad_utf8_1007() {
  let exec = TestExecutor::new();
  let (closed, client) = session(&exec, WsConfig::default(), echo);
  // A continuation frame with nothing to continue.
  write_raw(&exec, &client, raw_frame(0x0, b"x"));
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1002, "protocol error".into()))
  );
  assert_eq!(take(&closed), WsClose::ProtocolError);

  let (closed, client) = session(&exec, WsConfig::default(), echo);
  write_raw(&exec, &client, raw_frame(0x1, b"\xff\xfe"));
  assert_eq!(
    close_code(recv(&exec, &client)),
    Some((1007, "invalid utf-8".into()))
  );
  assert_eq!(take(&closed), WsClose::InvalidData);
}

// ── The HTTP upgrade, on a real listener ────────────────────────────────────────────────────

/// Serves `app` on an ephemeral port until the returned sender drops.
async fn listen(app: axum::Router) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (tx, rx) = tokio::sync::oneshot::channel::<()>();
  tokio::spawn(serve_listener(
    listener,
    app,
    ConnectionLimits::new(),
    async move {
      let _ = rx.await;
    },
  ));
  (addr, tx)
}

fn echo_route(_: &mut (), _: RequestCaps, socket: WsSocket) -> Effect<(), Never, ()> {
  let (incoming, out) = socket.into_parts::<Never, ()>();
  incoming.run_for_each_effect(move |msg| out.send(msg).catch(|_| succeed(())))
}

fn app(config: WsConfig) -> axum::Router {
  Router::new()
    .route(ws::endpoint("/echo", config, echo_route).public())
    .with_state(())
    .into_axum()
}

type TcpClient = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens `/echo` with the optional `Origin`; the HTTP status on refusal.
async fn connect(addr: SocketAddr, origin: Option<&str>) -> Result<TcpClient, StatusCode> {
  let mut req = format!("ws://{addr}/echo").into_client_request().unwrap();
  if let Some(origin) = origin {
    req
      .headers_mut()
      .insert("origin", HeaderValue::from_str(origin).unwrap());
  }
  match tokio_tungstenite::connect_async(req).await {
    Ok((ws, _)) => Ok(ws),
    Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Err(res.status()),
    Err(e) => panic!("unexpected handshake error: {e}"),
  }
}

async fn round_trip(ws: &mut TcpClient) {
  ws.send(Message::text("ping?")).await.unwrap();
  assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("ping?"));
}

#[tokio::test]
async fn r9_ws_upgrade_from_foreign_origin_refused() {
  let config = WsConfig::default()
    .allowed_origins(["https://app.example"])
    .unwrap();
  let (addr, _stop) = listen(app(config)).await;
  assert_eq!(
    connect(addr, Some("https://evil.example")).await.err(),
    Some(StatusCode::FORBIDDEN)
  );
  assert_eq!(
    connect(addr, Some("https://app.example.evil")).await.err(),
    Some(StatusCode::FORBIDDEN)
  );
  round_trip(&mut connect(addr, Some("https://app.example")).await.unwrap()).await;
  // Not a browser: no Origin at all.
  round_trip(&mut connect(addr, None).await.unwrap()).await;
  // The default allowlist is empty: every browser origin is refused.
  let (addr, _stop2) = listen(app(WsConfig::default())).await;
  assert_eq!(
    connect(addr, Some("http://localhost:3000")).await.err(),
    Some(StatusCode::FORBIDDEN)
  );
  // The dev profile allows the local origins by configuration.
  let dev = WsConfig::default()
    .allowed_origins(id_effect_axum::realtime::origin::DEV_ORIGINS)
    .unwrap();
  let (addr, _stop3) = listen(app(dev)).await;
  round_trip(&mut connect(addr, Some("http://localhost:5173")).await.unwrap()).await;
}

#[tokio::test]
async fn dangerous_any_origin_is_recorded() {
  let (addr, _stop) = listen(app(WsConfig::default().dangerous_any_origin())).await;
  assert!(
    id_effect::observability::posture::dangerous_opt_outs()
      .iter()
      .any(|(id, reopens)| *id == ANY_ORIGIN_OPT_OUT && reopens.contains("R9"))
  );
  round_trip(&mut connect(addr, Some("https://evil.example")).await.unwrap()).await;
}

#[tokio::test]
async fn r4_ws_sockets_per_ip_capped() {
  let (addr, _stop) = listen(app(WsConfig::default().max_sockets(100, 2))).await;
  let mut a = connect(addr, None).await.unwrap();
  let b = connect(addr, None).await.unwrap();
  assert_eq!(
    connect(addr, None).await.err(),
    Some(StatusCode::TOO_MANY_REQUESTS)
  );
  round_trip(&mut a).await;
  drop(b);
  // The slot is released once the server sees the socket go.
  let mut c = tokio::time::timeout(Duration::from_secs(10), async {
    loop {
      match connect(addr, None).await {
        Ok(ws) => break ws,
        Err(status) => {
          assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
          tokio::time::sleep(Duration::from_millis(10)).await;
        }
      }
    }
  })
  .await
  .expect("a closed socket frees its slot");
  round_trip(&mut c).await;
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

#[tokio::test]
async fn guarded_ws_route_authenticates_on_connect() {
  let config = WsConfig::default().protocols(["relay.v1", "relay.v0"]);
  let router = Router::new().route(ws::endpoint("/me", config, whoami).guard(KeyGuard));
  assert!(router.access_report().public.is_empty());
  let app = router
    .with_state(())
    .into_axum()
    .layer(CapOverrideLayer::new(|parts: &Parts| {
      let user = parts.headers.get("x-user")?.to_str().ok()?;
      Some(Principal(user.to_owned()))
    }));
  let (addr, _stop) = listen(app).await;
  let req = format!("ws://{addr}/me").into_client_request().unwrap();
  match tokio_tungstenite::connect_async(req).await {
    Err(tokio_tungstenite::tungstenite::Error::Http(res)) => {
      assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    other => panic!(
      "expected 401 before the upgrade, got {:?}",
      other.map(|_| ())
    ),
  }
  let mut req = format!("ws://{addr}/me").into_client_request().unwrap();
  let h = req.headers_mut();
  h.insert("x-key", HeaderValue::from_static("open-sesame"));
  h.insert("x-user", HeaderValue::from_static("ada"));
  h.insert(
    "sec-websocket-protocol",
    HeaderValue::from_static("other, relay.v0, relay.v1"),
  );
  let (mut ws, res) = tokio_tungstenite::connect_async(req).await.unwrap();
  assert_eq!(res.headers()["sec-websocket-protocol"], "relay.v1");
  assert_eq!(
    ws.next().await.unwrap().unwrap(),
    Message::text("ada relay.v1 127.0.0.1")
  );
  assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
}

#[tokio::test]
async fn non_upgrade_requests_are_refused_with_problems() {
  let app = app(WsConfig::default());
  let res = app
    .clone()
    .oneshot(Request::get("/echo").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
  assert_eq!(res.headers()["upgrade"], "websocket");
  let upgrade = |version: &'static str, key: &'static str| {
    Request::get("/echo")
      .header("connection", "Upgrade")
      .header("upgrade", "websocket")
      .header("sec-websocket-version", version)
      .header("sec-websocket-key", key)
      .body(Body::empty())
      .unwrap()
  };
  let res = app
    .clone()
    .oneshot(upgrade("8", "dGhlIHNhbXBsZSBub25jZQ=="))
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
  assert_eq!(res.headers()["sec-websocket-version"], "13");
  let res = app.clone().oneshot(upgrade("13", "short")).await.unwrap();
  assert_eq!(res.status(), StatusCode::BAD_REQUEST);
  // A well-formed handshake on a connection that cannot be upgraded (no hyper upgrade).
  let res = app
    .oneshot(upgrade("13", "dGhlIHNhbXBsZSBub25jZQ=="))
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
  assert_eq!(res.headers()["content-type"], "application/problem+json");
}
