//! Every procedure kind over WebTransport (r12-e12-3-webtransport, design note §5): one QUIC
//! stream per call carrying u32-BE length-prefixed records, the control stream, resume after a
//! lost session, the Origin allowlist on the CONNECT (ADR 0013 R9 / ADR 0014 C4), guards on the
//! CONNECT's head, the heartbeat and the stream rules. Real QUIC on loopback; the in-process
//! Rust WebTransport client (`id_effect_axum::server::webtransport::client`) on the other end.
#![cfg(feature = "webtransport")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::request::Parts;
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_axum::server::ConnectionLimits;
use id_effect_axum::server::h3::{Http3Options, serve_h3};
use id_effect_axum::server::webtransport::client::TlsConfig as ClientConfig;
use id_effect_axum::server::webtransport::client::{
  self, ConnectError, ConnectRequest, WebTransportClient,
};
use id_effect_axum::server::webtransport::{self as wt, WebTransportConfig, WebTransportStream};
use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};
use id_effect_platform::tls::{Identity, NodeCertificate, client_tls_config};
use id_effect_rpc::codec::postcard::{decode_frame, encode_record, split_record};
use id_effect_rpc::kinds::{Subscription, SubscriptionItem};
use id_effect_rpc::transport::webtransport::{self, RpcWebTransportConfig};
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::wire::{Cursor, FrameKind};
use id_effect_rpc::{Duplex, Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Generous: the suite runs beside compilations that load every core.
const WAIT: Duration = Duration::from_secs(20);

const APP: &str = "https://app.example";

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

/// Admits CONNECTs with `authorization: Bearer ok`.
struct Bearer;

impl RouteGuard for Bearer {
  fn policy(&self) -> &str {
    "bearer"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let ok = parts
      .headers
      .get("authorization")
      .is_some_and(|v| v == "Bearer ok");
    Box::pin(async move {
      if ok {
        Ok(())
      } else {
        Err(axum::response::IntoResponse::into_response(
          axum::http::StatusCode::FORBIDDEN,
        ))
      }
    })
  }
}

fn group(produced: Arc<AtomicUsize>) -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Greeter.greet").public(),
    text(),
    text(),
    |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
  );
  group.bidi(
    Procedure::bidi("Echo.double").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|items| items.map(|n| n * 2))),
  );
  group.server_stream(
    Procedure::server_stream("Count.forever").public(),
    int(),
    int(),
    move |_: i64, _: &mut DynEnv| {
      let produced = produced.clone();
      succeed::<_, Never, _>(Stream::unfold(0_i64, move |n| {
        produced.fetch_add(1, Ordering::SeqCst);
        Some((n, n + 1))
      }))
    },
  );
  group.subscription(
    Procedure::subscription("Feed.watch").public(),
    text(),
    text(),
    |room: String, after: Option<Cursor>, _: &mut DynEnv| {
      let from = after.map_or(0, |c| c.position + 1);
      let items = (from..from + 3)
        .map(|n| SubscriptionItem::new(format!("{room}#{n}")).with_cursor(Cursor::events(n)))
        .collect::<Vec<_>>();
      succeed::<_, Never, _>(Subscription::new(Stream::from_vec(items)))
    },
  );
  group.query(
    Procedure::query("Secret.read").guard(Bearer),
    text(),
    text(),
    |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
  );
  group
}

struct Fixture {
  addr: SocketAddr,
  tls: ClientConfig,
  config: RpcWebTransportConfig,
  _stop: tokio::sync::oneshot::Sender<()>,
}

fn default_config() -> RpcWebTransportConfig {
  let session = WebTransportConfig::default()
    .allowed_origins([APP])
    .unwrap();
  RpcWebTransportConfig::default().session(session)
}

fn serve(config: RpcWebTransportConfig) -> Fixture {
  let server = RpcServer::new(group(Arc::default()));
  let route = webtransport::endpoint(server.clone(), config.clone());
  let app = server
    .router()
    .route(route)
    .with_state(DynEnv::new())
    .into_axum();
  serve_app(app, config)
}

fn serve_app(app: axum::Router, config: RpcWebTransportConfig) -> Fixture {
  let cert = NodeCertificate::self_signed("localhost").unwrap();
  let pin = cert.cert_sha256();
  let identity = Identity::from_certificate(&cert).unwrap();
  let limits = ConnectionLimits::default();
  let quic = QuicEndpoint::bind(
    "127.0.0.1:0".parse().unwrap(),
    QuicEndpointConfig::new(identity.clone()).with_limits(wt::quic_limits(&limits)),
  )
  .unwrap();
  let addr = quic.local_addr();
  let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
  tokio::spawn(async move {
    let options = Http3Options::default().webtransport();
    serve_h3(&quic, app, limits, options, async {
      let _ = stopped.await;
    })
    .await
  });
  Fixture {
    addr,
    tls: client_tls_config(&identity, None, &[pin]).unwrap(),
    config,
    _stop: stop,
  }
}

fn from_app() -> ConnectRequest {
  ConnectRequest {
    origin: Some(APP.to_owned()),
    headers: Vec::new(),
  }
}

async fn connect(
  fx: &Fixture,
  path: &str,
  request: ConnectRequest,
) -> Result<WebTransportClient, ConnectError> {
  client::connect(fx.addr, "localhost", fx.tls.clone(), path, &request).await
}

/// One stream of frames, JSON records.
struct Frames {
  stream: WebTransportStream,
  buf: Vec<u8>,
}

impl Frames {
  async fn open(session: &WebTransportClient) -> Self {
    Self {
      stream: session.open_bi().await.unwrap(),
      buf: Vec::new(),
    }
  }

  async fn raw(&mut self, bytes: &[u8]) {
    self.stream.send.write_all(bytes).await.unwrap();
  }

  async fn send(&mut self, frame: Value) {
    let record = encode_record(frame.to_string().as_bytes()).unwrap();
    self.raw(&record).await;
  }

  async fn next_record(&mut self) -> Option<Vec<u8>> {
    loop {
      if let Some((record, used)) = split_record(&self.buf).unwrap() {
        let record = record.to_vec();
        self.buf.drain(..used);
        return Some(record);
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

  async fn next(&mut self) -> Option<Value> {
    let record = self.next_record().await?;
    Some(serde_json::from_slice(&record).unwrap())
  }

  async fn next_within(&mut self, wait: Duration) -> Option<Value> {
    tokio::time::timeout(wait, self.next()).await.ok().flatten()
  }

  /// The next frame of `kind`, skipping others.
  async fn next_kind(&mut self, kind: &str) -> Value {
    loop {
      let frame = self.next().await.expect("a frame");
      if frame["kind"] == kind {
        return frame;
      }
    }
  }
}

/// A session with its control stream open (a `ping` first) and the `hello` read.
async fn open_session(
  fx: &Fixture,
  request: ConnectRequest,
) -> (WebTransportClient, Frames, String) {
  let session = connect(fx, "/rpc/wt", request).await.unwrap();
  let mut control = Frames::open(&session).await;
  control.send(json!({"id": 0, "kind": "ping"})).await;
  let hello = control.next().await.expect("hello");
  assert_eq!(hello["kind"], "hello", "{hello}");
  assert_eq!(hello["payload"]["codec"], "idrpc.json.v1");
  let id = hello["payload"]["session"].as_str().unwrap().to_owned();
  assert_eq!(control.next().await.unwrap()["kind"], "pong");
  (session, control, id)
}

async fn closed(session: &WebTransportClient) {
  tokio::time::timeout(WAIT, session.closed())
    .await
    .expect("the session closes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webtransport_bidi_round_trip() {
  let fx = serve(default_config());
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  // A two-way call and a query, each on its own stream, at once.
  let mut echo = Frames::open(&session).await;
  echo
    .send(json!({"id": 1, "kind": "call", "tag": "Echo.double", "payload": null}))
    .await;
  let mut greet = Frames::open(&session).await;
  greet
    .send(json!({"id": 2, "kind": "call", "tag": "Greeter.greet", "payload": "Ada"}))
    .await;
  for n in 1..=10 {
    let flags = if n == 10 { 1 } else { 0 };
    echo
      .send(json!({"id": 1, "kind": "item", "flags": flags, "payload": n}))
      .await;
  }
  let exit = greet.next_kind("exit").await;
  assert_eq!(exit["payload"]["value"], "Hello, Ada!", "{exit}");
  // The server finishes a call's stream after its exit.
  assert!(greet.next().await.is_none());
  for n in 1..=10 {
    let item = echo.next_kind("item").await;
    assert_eq!(item["id"], 1);
    assert_eq!(item["payload"], n * 2);
  }
  let exit = echo.next_kind("exit").await;
  assert_eq!(exit["payload"]["_tag"], "Success", "{exit}");
  assert!(echo.next().await.is_none());
  // Nothing of the calls came on the control stream.
  assert!(
    control
      .next_within(Duration::from_millis(200))
      .await
      .is_none()
  );
  // Unknown procedures are refused per call, the session stays.
  let mut nope = Frames::open(&session).await;
  nope
    .send(json!({"id": 3, "kind": "call", "tag": "Nope.nope", "payload": 1}))
    .await;
  let exit = nope.next_kind("exit").await;
  assert_eq!(exit["payload"]["error"]["_tag"], "Rejected", "{exit}");
  assert_eq!(exit["payload"]["error"]["status"], 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webtransport_subscription_round_trip() {
  let fx = serve(default_config());
  let (session, _control, _) = open_session(&fx, from_app()).await;
  let mut feed = Frames::open(&session).await;
  feed
    .send(json!({"id": 1, "kind": "call", "tag": "Feed.watch", "payload": "a"}))
    .await;
  let mut cursors = Vec::new();
  let mut got = Vec::new();
  loop {
    let frame = feed.next().await.expect("a frame");
    match frame["kind"].as_str() {
      Some("item") => {
        got.push(frame["payload"].clone());
        cursors.push(frame["cursor"].as_str().unwrap().to_owned());
      }
      Some("exit") => break,
      _ => {}
    }
  }
  assert_eq!(got, [json!("a#0"), json!("a#1"), json!("a#2")]);
  // A new call resumes after the last event cursor.
  let mut resumed = Frames::open(&session).await;
  resumed
    .send(
      json!({"id": 2, "kind": "call", "tag": "Feed.watch", "payload": "a", "cursor": cursors[2]}),
    )
    .await;
  let mut got = Vec::new();
  loop {
    let frame = resumed.next().await.expect("a frame");
    match frame["kind"].as_str() {
      Some("item") => got.push(frame["payload"].clone()),
      Some("exit") => break,
      _ => {}
    }
  }
  assert_eq!(got, [json!("a#3"), json!("a#4"), json!("a#5")]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webtransport_resume_after_session_loss() {
  let fx = serve(default_config());
  let (session, _control, id) = open_session(&fx, from_app()).await;
  let mut count = Frames::open(&session).await;
  count
    .send(json!({"id": 1, "kind": "call", "tag": "Count.forever", "payload": 0}))
    .await;
  let mut cursor = String::new();
  for n in 0..5 {
    let item = count.next_kind("item").await;
    assert_eq!(item["payload"], n);
    cursor = item["cursor"].as_str().unwrap().to_owned();
  }
  // The session is lost (the network went away): the stream keeps running detached.
  session.close(0, "lost");
  closed(&session).await;
  tokio::time::timeout(WAIT, async {
    while fx.config.rpc_config().resume_stats().detached == 0 {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .expect("the session detaches");
  // A new session resumes it on its control stream.
  let session = connect(&fx, "/rpc/wt", from_app()).await.unwrap();
  let mut control = Frames::open(&session).await;
  control
    .send(json!({"id": 0, "kind": "resume", "payload": {"session": id, "calls": [{"id": 1, "cursor": cursor}]}}))
    .await;
  let first = control.next_kind("hello").await;
  assert_ne!(first["payload"]["session"], id.as_str());
  let hello = control.next_kind("hello").await;
  assert_eq!(hello["payload"]["session"], id.as_str(), "resumed");
  // The replay after the cursor, in order, on the control stream (the call has no stream here).
  let mut next = 5;
  while let Some(frame) = control.next_within(Duration::from_millis(500)).await {
    if frame["kind"] == "item" {
      assert_eq!(frame["id"], 1);
      assert_eq!(frame["payload"], next, "{frame}");
      next += 1;
    }
  }
  assert!(next > 5, "replayed items");
  // The call moves to a stream of its own with its next frame: the credits.
  let mut moved = Frames::open(&session).await;
  moved
    .send(json!({"id": 1, "kind": "credit", "credits": 32}))
    .await;
  let item = moved.next_kind("item").await;
  assert_eq!(item["payload"], next, "{item}");
  // Cancel ends it there.
  moved.send(json!({"id": 1, "kind": "cancel"})).await;
  let exit = moved.next_kind("exit").await;
  assert_eq!(exit["payload"]["_tag"], "Failure", "{exit}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r9_webtransport_connect_from_foreign_origin_refused() {
  let fx = serve(default_config());
  let foreign = ConnectRequest {
    origin: Some("https://evil.example".to_owned()),
    headers: Vec::new(),
  };
  assert_eq!(
    connect(&fx, "/rpc/wt", foreign).await.unwrap_err(),
    ConnectError::Refused(403)
  );
  // The allowed origin, and a non-browser client (no Origin), are served.
  let (session, _, _) = open_session(&fx, from_app()).await;
  session.close(0, "done");
  let (session, _, _) = session_without_origin(&fx).await;
  session.close(0, "done");
  // The default allowlist is empty: every browser origin is refused.
  let strict = serve(RpcWebTransportConfig::default());
  assert_eq!(
    connect(&strict, "/rpc/wt", from_app()).await.unwrap_err(),
    ConnectError::Refused(403)
  );
}

async fn session_without_origin(fx: &Fixture) -> (WebTransportClient, Frames, String) {
  open_session(fx, ConnectRequest::default()).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guards_see_the_connect_and_codecs_are_negotiated() {
  let fx = serve(default_config());
  let secret = json!({"id": 1, "kind": "call", "tag": "Secret.read", "payload": "s"});
  // Without the bearer on the CONNECT: refused per call.
  let (session, _, _) = open_session(&fx, from_app()).await;
  let mut call = Frames::open(&session).await;
  call.send(secret.clone()).await;
  let exit = call.next_kind("exit").await;
  assert_eq!(exit["payload"]["error"]["status"], 403, "{exit}");
  // With it: served.
  let bearer = ConnectRequest {
    origin: Some(APP.to_owned()),
    headers: vec![("authorization".to_owned(), "Bearer ok".to_owned())],
  };
  let (session, _, _) = open_session(&fx, bearer).await;
  let mut call = Frames::open(&session).await;
  call.send(secret).await;
  let exit = call.next_kind("exit").await;
  assert_eq!(exit["payload"]["value"], "s", "{exit}");
  // Postcard by the query; an unknown codec is refused before the session starts.
  let session = connect(&fx, "/rpc/wt?codec=idrpc.postcard.v1", from_app())
    .await
    .unwrap();
  let mut control = Frames::open(&session).await;
  let ping = id_effect_rpc::codec::postcard::encode_frame(&id_effect_rpc::wire::Frame::new(
    id_effect_rpc::wire::CallId::CONNECTION,
    FrameKind::Ping,
  ))
  .unwrap();
  control.raw(&encode_record(&ping).unwrap()).await;
  let hello = decode_frame(&control.next_record().await.unwrap()).unwrap();
  assert_eq!(hello.kind, FrameKind::Hello);
  assert_eq!(
    connect(&fx, "/rpc/wt?codec=cbor", from_app())
      .await
      .unwrap_err(),
    ConnectError::Refused(415)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_that_break_the_rules_close_the_session() {
  let fx = serve(default_config());
  // A second control stream.
  let (session, _control, _) = open_session(&fx, from_app()).await;
  let mut second = Frames::open(&session).await;
  second.send(json!({"id": 0, "kind": "ping"})).await;
  closed(&session).await;
  // A frame for another call on a call's stream.
  let (session, _control, _) = open_session(&fx, from_app()).await;
  let mut call = Frames::open(&session).await;
  call
    .send(json!({"id": 1, "kind": "call", "tag": "Echo.double", "payload": null}))
    .await;
  call
    .send(json!({"id": 2, "kind": "item", "payload": 1}))
    .await;
  closed(&session).await;
  // A record over 64 KiB, refused from its length prefix alone.
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  control.raw(&(70_000_u32).to_be_bytes()).await;
  closed(&session).await;
  // An undecodable frame.
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  control.raw(&encode_record(b"not json").unwrap()).await;
  closed(&session).await;
  // A frame the connection logic refuses (a client may not send `hello`) ends the session too.
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  control.send(json!({"id": 0, "kind": "hello"})).await;
  closed(&session).await;
  // A stream that ends inside a record.
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  control.raw(&[0, 0, 0, 9, b'{']).await;
  control.stream.send.shutdown().await.unwrap();
  closed(&session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_heartbeat_pings_and_closes_silent_sessions() {
  let config = default_config().heartbeat(Duration::from_millis(150), 2);
  assert_eq!(config.heartbeat_settings(), (Duration::from_millis(150), 2));
  let fx = serve(config);
  // A client that opens its control stream and then says nothing: pinged, then closed.
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  let ping = control.next_kind("ping").await;
  assert_eq!(ping["id"], 0);
  closed(&session).await;
  // A client that never opens its control stream is closed too.
  let session = connect(&fx, "/rpc/wt", from_app()).await.unwrap();
  closed(&session).await;
  // One that answers stays.
  let (session, mut control, _) = open_session(&fx, from_app()).await;
  for _ in 0..4 {
    control.next_kind("ping").await;
    control.send(json!({"id": 0, "kind": "pong"})).await;
  }
  let mut greet = Frames::open(&session).await;
  greet
    .send(json!({"id": 1, "kind": "call", "tag": "Greeter.greet", "payload": "B"}))
    .await;
  assert_eq!(
    greet.next_kind("exit").await["payload"]["value"],
    "Hello, B!"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_streams_are_bounded_by_the_call_cap() {
  // One call at once: at most 2 + 16 streams bound to calls.
  let config = default_config().rpc(RpcWsConfig::default().max_calls(1));
  let fx = serve(config);
  let (session, _control, _) = open_session(&fx, from_app()).await;
  let mut streams = Vec::new();
  for id in 1..=19 {
    let mut stream = Frames::open(&session).await;
    stream
      .send(json!({"id": id, "kind": "credit", "credits": 1}))
      .await;
    streams.push(stream);
  }
  closed(&session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sessions_are_served_on_a_route_of_your_own() {
  let server = RpcServer::new(group(Arc::default()));
  let config = default_config();
  let route = {
    let server = server.clone();
    let config = config.clone();
    wt::endpoint(
      "/own",
      config.session_config().clone(),
      move |env: &mut DynEnv, _caps, session| {
        let head = axum::http::Request::new(()).into_parts().0;
        webtransport::serve(
          &server,
          &config,
          env.clone(),
          head,
          id_effect_rpc::codec::CodecId::Json,
          session,
        )
      },
    )
    .public()
  };
  let app = server
    .router()
    .route(route)
    .with_state(DynEnv::new())
    .into_axum();
  let fx = serve_app(app, config);
  let session = connect(&fx, "/own", from_app()).await.unwrap();
  let mut control = Frames::open(&session).await;
  control.send(json!({"id": 0, "kind": "ping"})).await;
  assert_eq!(control.next().await.unwrap()["kind"], "hello");
  let mut greet = Frames::open(&session).await;
  greet
    .send(json!({"id": 1, "kind": "call", "tag": "Greeter.greet", "payload": "C"}))
    .await;
  assert_eq!(
    greet.next_kind("exit").await["payload"]["value"],
    "Hello, C!"
  );
}

#[test]
fn the_route_is_a_declared_public_connect() {
  let server = RpcServer::new(group(Arc::default()));
  let config = RpcWebTransportConfig::new().rpc(RpcWsConfig::default().max_calls(8));
  assert_eq!(config.rpc_config().calls_limit(), 8);
  let route = webtransport::endpoint(server.clone().with_path("/api"), config);
  let app = server.router().route(route);
  assert!(
    app
      .access_report()
      .public
      .contains(&"CONNECT /api/wt".to_owned())
  );
}
