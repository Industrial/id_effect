//! Two-way streams over WebSocket (web.rpc#4, r12-e12-2-bidi-resume): credit backpressure in
//! both directions bounds the memory of a stream by the credit window, and a two-way call's
//! input carries on after a resume. Real sockets on loopback, raw frames on the client side.
#![cfg(feature = "ws")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::realtime::ws::WsConfig;
use id_effect_rpc::server::DEFAULT_STREAM_WINDOW;
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::{Duplex, Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn group(produced: Arc<AtomicUsize>) -> RpcGroup {
  let mut group = RpcGroup::new();
  group.bidi(
    Procedure::bidi("Echo.double").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|items| items.map(|n| n * 2))),
  );
  // Produces without end and holds its input unread; counts what it produced.
  group.bidi(
    Procedure::bidi("Echo.flood").public(),
    int(),
    int(),
    move |_: &mut DynEnv| {
      let produced = produced.clone();
      succeed::<_, Never, _>(Duplex::new(move |items| {
        Stream::unfold(0_i64, move |n| {
          let _held = &items;
          produced.fetch_add(1, Ordering::SeqCst);
          Some((n, n + 1))
        })
      }))
    },
  );
  group
}

fn config() -> RpcWsConfig {
  let socket = WsConfig::default()
    .messages_per_second(100_000)
    .max_sockets(1000, 1000);
  RpcWsConfig::default().socket(socket)
}

async fn serve(config: RpcWsConfig, produced: Arc<AtomicUsize>) -> SocketAddr {
  let server = RpcServer::new(group(produced)).with_websocket(config);
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

struct Client {
  sink: SplitSink<Socket, Message>,
  stream: SplitStream<Socket>,
  session: String,
}

impl Client {
  async fn connect(addr: SocketAddr) -> Self {
    let mut request = format!("ws://{addr}/rpc/ws").into_client_request().unwrap();
    request
      .headers_mut()
      .insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let (sink, stream) = socket.split();
    let mut client = Self {
      sink,
      stream,
      session: String::new(),
    };
    let hello = client.next().await.expect("hello");
    client.session = hello["payload"]["session"].as_str().unwrap().to_owned();
    client
  }

  async fn next(&mut self) -> Option<Value> {
    loop {
      match tokio::time::timeout(Duration::from_secs(10), self.stream.next())
        .await
        .ok()??
      {
        Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).ok(),
        Ok(Message::Close(_)) | Err(_) => return None,
        Ok(_) => {}
      }
    }
  }

  async fn next_within(&mut self, wait: Duration) -> Option<Value> {
    tokio::time::timeout(wait, self.next()).await.ok().flatten()
  }

  async fn send(&mut self, frame: Value) {
    self
      .sink
      .send(Message::Text(frame.to_string().into()))
      .await
      .unwrap();
  }

  /// The next frame of `kind` (skipping others).
  async fn next_kind(&mut self, kind: &str) -> Value {
    loop {
      let frame = self.next().await.expect("a frame");
      if frame["kind"] == kind {
        return frame;
      }
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidi_backpressure_bounds_memory() {
  let config = config();
  let produced = Arc::new(AtomicUsize::new(0));
  let addr = serve(config.clone(), produced.clone()).await;
  let mut client = Client::connect(addr).await;
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Echo.flood", "payload": null}))
    .await;
  // The window, then nothing until a credit.
  for n in 0..32 {
    assert_eq!(client.next_kind("item").await["payload"], n);
  }
  assert!(
    client
      .next_within(Duration::from_millis(200))
      .await
      .is_none()
  );
  // The producer is suspended: past the window, at most the call's item queue and the chunk
  // the stream pulls at once; and it stays there.
  let bound = 32 + DEFAULT_STREAM_WINDOW + 64;
  let first = produced.load(Ordering::SeqCst);
  assert!(first <= bound, "produced {first} > {bound}");
  tokio::time::sleep(Duration::from_millis(100)).await;
  assert_eq!(
    produced.load(Ordering::SeqCst),
    first,
    "a suspended producer"
  );
  // The unacknowledged window is all the session holds.
  let held = config.resume_stats();
  assert_eq!((held.calls, held.messages), (1, 32));
  // Credits release the buffer and move the stream on by exactly as much.
  for round in 1..=3_i64 {
    client
      .send(json!({"id": 1, "kind": "credit", "credits": 32}))
      .await;
    for n in 0..32 {
      assert_eq!(client.next_kind("item").await["payload"], round * 32 + n);
    }
    assert!(
      client
        .next_within(Duration::from_millis(50))
        .await
        .is_none()
    );
    assert_eq!(config.resume_stats().messages, 32);
    let now = produced.load(Ordering::SeqCst);
    assert!(now <= first + (round as usize) * 32 + 64, "produced {now}");
  }
  // The client side: past its credits (the window, and what the unread input queue took),
  // the call fails with ResourceExhausted.
  for n in 0..(32 + DEFAULT_STREAM_WINDOW as i64 + 64) {
    client
      .send(json!({"id": 1, "kind": "item", "payload": n}))
      .await;
  }
  let exit = client.next_kind("exit").await;
  assert_eq!(
    exit["payload"]["error"]["_tag"], "ResourceExhausted",
    "{exit}"
  );
  assert_eq!(exit["payload"]["error"]["limit"], "credits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidi_input_carries_on_after_a_resume() {
  let config = config();
  let addr = serve(config.clone(), Arc::default()).await;
  let mut client = Client::connect(addr).await;
  let session = client.session.clone();
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Echo.double", "payload": null}))
    .await;
  let mut cursor = String::new();
  for n in 1..=3 {
    client
      .send(json!({"id": 1, "kind": "item", "payload": n}))
      .await;
    let item = client.next_kind("item").await;
    assert_eq!(item["payload"], n * 2);
    cursor = item["cursor"].as_str().unwrap().to_owned();
  }
  drop(client);
  let mut client = Client::connect(addr).await;
  client
    .send(json!({"id": 0, "kind": "resume", "payload": {"session": session, "calls": [{"id": 1, "cursor": cursor}]}}))
    .await;
  let hello = client.next().await.unwrap();
  assert_eq!(hello["payload"]["session"], session.as_str());
  // The send window restarts: 32 less the three items the handler took.
  let credit = client.next().await.unwrap();
  assert_eq!(
    (credit["kind"].as_str(), credit["credits"].as_u64()),
    (Some("credit"), Some(29))
  );
  client
    .send(json!({"id": 1, "kind": "item", "payload": 4}))
    .await;
  client
    .send(json!({"id": 1, "kind": "item", "flags": 1, "payload": 5}))
    .await;
  assert_eq!(client.next_kind("item").await["payload"], 8);
  assert_eq!(client.next_kind("item").await["payload"], 10);
  let exit = client.next_kind("exit").await;
  assert_eq!(exit["payload"]["_tag"], "Success", "{exit}");
}
