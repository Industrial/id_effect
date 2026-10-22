//! F39 (r13-f-ws-credit): `credit` and `ping` frames leave the socket's message rate and count
//! against their own finite cap (ADR 0013 R4), so a client reading a fast stream is not throttled
//! by its own credits. Real sockets on loopback: raw frames from tokio-tungstenite, and the Rust
//! client (`client::ws`).
#![cfg(all(feature = "ws", feature = "client"))]

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::realtime::ws::WsConfig;
use id_effect_rpc::client::CallEvent;
use id_effect_rpc::client::ws::WsClient;
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Raw = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Greeter.greet").public(),
    text(),
    text(),
    |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
  );
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.client_stream(
    Procedure::client_stream("Sum.all").public(),
    int(),
    int(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  group
}

async fn serve(config: RpcWsConfig) -> SocketAddr {
  let server = RpcServer::new(group()).with_websocket(config);
  let app = server.router().with_state(DynEnv::new()).into_axum();
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(
      listener,
      app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
  });
  addr
}

fn url(addr: SocketAddr) -> String {
  format!("ws://{addr}/rpc/ws")
}

/// What a raw client read: the frames, and the close frame's code and reason (if any).
#[derive(Debug, Default)]
struct Read {
  frames: Vec<Value>,
  close: Option<(u16, String)>,
}

impl Read {
  fn count(&self, kind: &str) -> usize {
    self.frames.iter().filter(|f| f["kind"] == kind).count()
  }
}

/// A raw JSON-codec socket, past the server's `hello`.
async fn raw(addr: SocketAddr) -> Raw {
  let mut request = url(addr).into_client_request().unwrap();
  request
    .headers_mut()
    .insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
  let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
  let hello = read(&mut socket, |r| !r.frames.is_empty()).await;
  assert_eq!(hello.frames[0]["kind"], "hello");
  socket
}

/// Sends every frame without waiting in between.
async fn send_all(socket: &mut Raw, frames: impl IntoIterator<Item = Value>) {
  for frame in frames {
    socket
      .feed(Message::Text(frame.to_string().into()))
      .await
      .unwrap();
  }
  socket.flush().await.unwrap();
}

/// Reads frames until `done` holds, the server closes, or 10 s pass.
async fn read(socket: &mut Raw, done: impl Fn(&Read) -> bool) -> Read {
  let mut out = Read::default();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  while !done(&out) {
    let Ok(Some(Ok(msg))) = tokio::time::timeout_at(deadline, socket.next()).await else {
      break;
    };
    match msg {
      Message::Text(text) => out.frames.push(serde_json::from_str(&text).unwrap()),
      Message::Close(frame) => {
        out.close = frame.map(|f| (u16::from(f.code), f.reason.to_string()));
        break;
      }
      _ => {}
    }
  }
  out
}

fn credit(id: u64) -> Value {
  json!({"id": id, "kind": "credit", "credits": 1})
}

fn ping() -> Value {
  json!({"id": 0, "kind": "ping"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn credit_frames_do_not_throttle_data() {
  // Four data messages per second (burst 4); the control cap defaults to 16.
  let socket = WsConfig::default().messages_per_second(4);
  let config = RpcWsConfig::default().socket(socket);
  assert_eq!(config.socket_config().control_message_rate(), 16);
  let addr = serve(config).await;
  let mut socket = raw(addr).await;
  // At once: four data frames (the whole burst) and fifteen control frames beside them. Counted
  // together they were nineteen messages against a rate of four, and the socket closed 1008.
  let mut frames = vec![json!({"id": 1, "kind": "call", "tag": "Count.up", "payload": 3})];
  frames.extend((0..12).map(|_| credit(1)));
  frames.extend((0..3).map(|_| ping()));
  frames.extend((2..=4).map(
    |id| json!({"id": id, "kind": "call", "tag": "Greeter.greet", "payload": format!("n{id}")}),
  ));
  send_all(&mut socket, frames).await;
  let got = read(&mut socket, |r| {
    r.count("exit") == 4 && r.count("pong") == 3
  })
  .await;
  assert_eq!(got.close, None, "the socket stayed open: {got:?}");
  let items = got
    .frames
    .iter()
    .filter(|f| f["kind"] == "item")
    .map(|f| f["payload"].clone())
    .collect::<Vec<_>>();
  assert_eq!(items, [json!(1), json!(2), json!(3)]);
  for id in 2..=4 {
    let exit = got
      .frames
      .iter()
      .find(|f| f["kind"] == "exit" && f["id"] == id)
      .expect("an exit per call");
    assert_eq!(exit["payload"]["value"], json!(format!("Hello, n{id}!")));
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_client_stream_at_rate_limit_completes() {
  // A window of 2: the client grants a credit back for every item it reads.
  let socket = WsConfig::default().messages_per_second(16);
  let addr = serve(RpcWsConfig::default().window(2).socket(socket)).await;
  let client = WsClient::connect(&url(addr)).await.unwrap();
  assert_eq!(client.hello().window, 2);
  // Read a stream as fast as it comes: one data frame (the call), 48 credits.
  let mut up = client.open("Count.up", json!(48), None).await.unwrap();
  let mut got = Vec::new();
  loop {
    match up.next().await.expect("an exit") {
      CallEvent::Item { value, .. } => got.push(value),
      CallEvent::Exit(exit) => {
        assert_eq!(exit.unwrap(), Value::Null);
        break;
      }
    }
  }
  assert_eq!(got, (1..=48).map(Value::from).collect::<Vec<_>>());
  // Then a client stream at the rate: call, 13 items and the half-close are the 15 data frames
  // that bring the socket to its burst of 16.
  let sum = client
    .client_stream("Sum.all", (1..=13).map(Value::from))
    .await
    .unwrap();
  assert_eq!(sum, json!(91));
  // The socket is still open once the rate has room again.
  tokio::time::sleep(Duration::from_millis(250)).await;
  let greet = client.call("Greeter.greet", json!("Ada")).await.unwrap();
  assert_eq!(greet, json!("Hello, Ada!"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r4_control_frames_have_own_cap() {
  // Data rate 2 per second, so the control cap is 8 per second (bursts of 8).
  let socket = WsConfig::default().messages_per_second(2);
  let config = RpcWsConfig::default().socket(socket);
  assert_eq!(config.socket_config().control_message_rate(), 8);
  let addr = serve(config).await;
  // A credit flood closes the socket with 1008, as data over the rate does.
  let mut socket = raw(addr).await;
  send_all(&mut socket, (0..40).map(|_| credit(1))).await;
  let got = read(&mut socket, |_| false).await;
  assert_eq!(
    got.close,
    Some((1008, "control frame rate exceeded".to_owned()))
  );
  // So does an application ping flood: the socket closes long before every ping is answered.
  let mut socket = raw(addr).await;
  send_all(&mut socket, (0..40).map(|_| ping())).await;
  let got = read(&mut socket, |_| false).await;
  assert_eq!(
    got.close,
    Some((1008, "control frame rate exceeded".to_owned()))
  );
  let pongs = got.count("pong");
  assert!(pongs < 40, "{pongs} pongs");
  // An explicit cap replaces the default: 2 control frames, the third closes.
  let socket = WsConfig::default()
    .messages_per_second(100)
    .control_messages_per_second(2);
  let addr = serve(RpcWsConfig::default().socket(socket)).await;
  let mut socket = raw(addr).await;
  send_all(&mut socket, (0..20).map(|_| ping())).await;
  let got = read(&mut socket, |_| false).await;
  assert_eq!(
    got.close,
    Some((1008, "control frame rate exceeded".to_owned()))
  );
}
