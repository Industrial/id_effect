//! Every procedure kind over one WebSocket (design note §5), with credit backpressure, the
//! per-connection call cap (ADR 0014 C3), guards on the upgrade request, and the frame protocol's
//! refusals. Real sockets on loopback; the Rust client (`client::ws`) on the other end.
#![cfg(all(feature = "ws", feature = "client"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::request::Parts;
use futures_util::{SinkExt as _, StreamExt as _};
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_rpc::client::ws::WsClient;
use id_effect_rpc::client::{CallEvent, RpcClient, RpcClientError, TransportKind};
use id_effect_rpc::kinds::{Subscription, SubscriptionItem};
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::wire::Cursor;
use id_effect_rpc::{Duplex, Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

/// Admits requests with `authorization: Bearer ok`.
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

struct Fixture {
  addr: SocketAddr,
  produced: Arc<AtomicUsize>,
}

fn group(produced: Arc<AtomicUsize>) -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Greeter.greet").public(),
    text(),
    text(),
    |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
  );
  group.mutation(
    Procedure::mutation("Counter.add").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n + 1),
  );
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
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
  group.client_stream(
    Procedure::client_stream("Sum.all").public(),
    int(),
    int(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  group.client_stream(
    Procedure::client_stream("Sink.stall").public(),
    int(),
    int(),
    // Holds its input without reading it.
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| {
      id_effect::from_async(move || async move {
        let _held = items;
        std::future::pending::<Result<i64, Never>>().await
      })
    },
  );
  group.bidi(
    Procedure::bidi("Echo.double").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|items| items.map(|n| n * 2))),
  );
  group.bidi(
    Procedure::bidi("Echo.ignore").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|_items| Stream::from_iterable(0..50))),
  );
  group.bidi(
    Procedure::bidi("Echo.hold").public(),
    int(),
    int(),
    |_: &mut DynEnv| {
      succeed::<_, Never, _>(Duplex::new(|items| {
        Stream::from_pull(move |_| {
          let _held = &items;
          id_effect::from_async(std::future::pending::<Result<Option<Vec<i64>>, Never>>)
        })
      }))
    },
  );
  group.server_stream(
    Procedure::server_stream("Big.blob").public(),
    int(),
    text(),
    |n: i64, _: &mut DynEnv| {
      succeed::<_, Never, _>(Stream::from_iterable([
        "small".to_owned(),
        "x".repeat(n as usize),
      ]))
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
  group.subscription(
    Procedure::subscription("Feed.idle").public(),
    text(),
    text(),
    |_: String, _, _: &mut DynEnv| {
      succeed::<_, Never, _>(Subscription::from_stream(Stream::from_pull(|_| {
        id_effect::from_async(std::future::pending::<Result<Option<Vec<String>>, Never>>)
      })))
    },
  );
  #[cfg(feature = "postcard")]
  group.query(
    Procedure::query("Typed.add").public(),
    int().with_serde(),
    int().with_serde(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n + 1),
  );
  #[cfg(feature = "postcard")]
  group.client_stream(
    Procedure::client_stream("Typed.sum").public(),
    int().with_serde(),
    int().with_serde(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  // Typed input, untyped output: postcard can call it but not carry its answer.
  #[cfg(feature = "postcard")]
  group.query(
    Procedure::query("Half.query").public(),
    int().with_serde(),
    text(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n.to_string()),
  );
  #[cfg(feature = "postcard")]
  group.server_stream(
    Procedure::server_stream("Half.stream").public(),
    int().with_serde(),
    text(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable([n.to_string()])),
  );
  group.query(
    Procedure::query("Secret.read").guard(Bearer),
    text(),
    text(),
    |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
  );
  group
}

async fn serve(config: RpcWsConfig) -> Fixture {
  let produced = Arc::new(AtomicUsize::new(0));
  let server = RpcServer::new(group(produced.clone())).with_websocket(config);
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
  Fixture { addr, produced }
}

fn url(addr: SocketAddr) -> String {
  format!("ws://{addr}/rpc/ws")
}

async fn items(
  call: &mut id_effect_rpc::client::ws::WsCall,
) -> (Vec<Value>, Result<Value, RpcClientError>) {
  let mut out = Vec::new();
  loop {
    match call.next().await {
      Some(CallEvent::Item { value, .. }) => out.push(value),
      Some(CallEvent::Exit(result)) => return (out, result),
      None => panic!("no exit"),
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_kind_over_websocket() {
  // The realtime layer's default of 100 inbound messages per second would close this socket.
  let socket = id_effect_axum::realtime::ws::WsConfig::default().messages_per_second(10_000);
  let fx = serve(RpcWsConfig::default().socket(socket)).await;
  let client = WsClient::connect(&url(fx.addr)).await.unwrap();
  assert_eq!(client.hello().codec, "idrpc.json.v1");
  assert_eq!(
    (client.hello().window, client.hello().max_frame),
    (32, 65536)
  );
  assert!(format!("{client:?}").contains("hello"));

  // Query and mutation, concurrently on one socket.
  let (greet, add) = futures_util::future::join(
    client.call("Greeter.greet", json!("Ada")),
    client.call("Counter.add", json!(41)),
  )
  .await;
  assert_eq!(greet.unwrap(), json!("Hello, Ada!"));
  assert_eq!(add.unwrap(), json!(42));

  // Server stream: more items than the credit window.
  let mut up = client.open("Count.up", json!(100), None).await.unwrap();
  assert!(format!("{up:?}").contains("WsCall"));
  let (got, exit) = items(&mut up).await;
  assert_eq!(got, (1..=100).map(Value::from).collect::<Vec<_>>());
  assert_eq!(exit.unwrap(), Value::Null);
  assert!(up.next().await.is_none());

  // Client stream: more items than the window, so credits come back.
  let sum = client
    .client_stream("Sum.all", (1..=100).map(Value::from))
    .await
    .unwrap();
  assert_eq!(sum, json!(5050));

  // Two-way stream.
  let mut echo = client.open("Echo.double", Value::Null, None).await.unwrap();
  for n in 1..=40 {
    echo.send(json!(n)).await.unwrap();
  }
  echo.end().await.unwrap();
  let (got, exit) = items(&mut echo).await;
  assert_eq!(
    got,
    (1..=40).map(|n| Value::from(n * 2)).collect::<Vec<_>>()
  );
  assert!(exit.is_ok());

  // Subscription, then resumed after a cursor.
  let mut feed = client.open("Feed.watch", json!("a"), None).await.unwrap();
  let mut cursors = Vec::new();
  while let Some(event) = feed.next().await {
    if let CallEvent::Item { cursor, .. } = event {
      cursors.push(cursor.unwrap());
    }
  }
  assert_eq!(cursors.last(), Some(&Cursor::events(2)));
  let mut resumed = client
    .open("Feed.watch", json!("a"), Some(Cursor::events(2)))
    .await
    .unwrap();
  let (got, _) = items(&mut resumed).await;
  assert_eq!(got, [json!("a#3"), json!("a#4"), json!("a#5")]);

  // Typed failures and refusals arrive as exits.
  let bad = client.call("Counter.add", json!("x")).await.unwrap_err();
  assert_eq!(bad.failure_tag(), Some("InvalidInput"));
  let unknown = client.call("Nope.nope", json!(1)).await.unwrap_err();
  assert_eq!(unknown.failure_tag(), Some("Rejected"));
  let RpcClientError::Failure(error) = unknown else {
    panic!("a failure");
  };
  assert_eq!(error["status"], 404);
  let refused = client.call("Secret.read", json!("s")).await.unwrap_err();
  let RpcClientError::Failure(error) = refused else {
    panic!("a failure");
  };
  assert_eq!(error["status"], 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guards_see_the_upgrade_request() {
  let fx = serve(RpcWsConfig::default()).await;
  let mut request = url(fx.addr).into_client_request().unwrap();
  let headers = request.headers_mut();
  headers.insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
  headers.insert("authorization", "Bearer ok".parse().unwrap());
  let (socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
  assert_eq!(
    response.headers()["sec-websocket-protocol"],
    "idrpc.json.v1"
  );
  let client = WsClient::over(socket).await.unwrap();
  assert_eq!(client.call("Secret.read", json!("s")).await.unwrap(), "s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_ws_client_applies_backpressure() {
  let fx = serve(RpcWsConfig::default()).await;
  let client = WsClient::connect(&url(fx.addr)).await.unwrap();
  let mut call = client.open("Count.forever", json!(0), None).await.unwrap();
  // Read nothing: the producer stops after the credits, the window and one chunk.
  tokio::time::sleep(Duration::from_millis(300)).await;
  let stalled = fx.produced.load(Ordering::SeqCst);
  assert!(stalled > 0, "the stream started");
  assert!(stalled <= 200, "producer ran ahead: {stalled} items");
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(fx.produced.load(Ordering::SeqCst), stalled, "still waiting");
  // Reading grants credits back, so the producer moves on.
  for expected in 0..500 {
    match call.next().await {
      Some(CallEvent::Item { value, .. }) => assert_eq!(value, json!(expected)),
      other => panic!("unexpected {other:?}"),
    }
  }
  assert!(fx.produced.load(Ordering::SeqCst) > stalled + 300);
  // Another call on the same socket is not held up by the stalled stream.
  assert_eq!(client.call("Counter.add", json!(1)).await.unwrap(), 2);
  call.cancel().await.unwrap();
  let (_, exit) = items(&mut call).await;
  assert_eq!(exit.unwrap_err().failure_tag(), Some("Cancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r4_ws_streams_per_connection_capped() {
  let fx = serve(RpcWsConfig::default().max_calls(2)).await;
  let client = WsClient::connect(&url(fx.addr)).await.unwrap();
  let first = client.open("Feed.idle", json!("a"), None).await.unwrap();
  let mut second = client.open("Feed.idle", json!("b"), None).await.unwrap();
  let third = client.call("Greeter.greet", json!("c")).await.unwrap_err();
  assert_eq!(
    third.reserved(),
    Some(id_effect_rpc::wire::RpcFailure::ResourceExhausted {
      limit: "calls".into()
    })
  );
  // Ending one frees its slot.
  second.cancel().await.unwrap();
  let (_, exit) = items(&mut second).await;
  assert_eq!(exit.unwrap_err().failure_tag(), Some("Cancelled"));
  let mut answer = Err(RpcClientError::Transport("not yet".into()));
  for _ in 0..50 {
    answer = client.call("Greeter.greet", json!("d")).await;
    if answer.is_ok() {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert_eq!(answer.unwrap(), "Hello, d!");
  // Dropping a call cancels it on the server too.
  drop(first);
  let mut freed = false;
  for _ in 0..50 {
    let a = client.open("Feed.idle", json!("e"), None).await.unwrap();
    let b = client.call("Greeter.greet", json!("f")).await;
    drop(a);
    if b.is_ok() {
      freed = true;
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert!(freed);
}

async fn raw(
  addr: SocketAddr,
) -> (
  futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
  >,
  futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
  >,
) {
  let mut request = url(addr).into_client_request().unwrap();
  request
    .headers_mut()
    .insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
  let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
  let (sink, mut stream) = socket.split();
  let hello = next_json(&mut stream).await.unwrap();
  assert_eq!(hello["kind"], "hello");
  assert_eq!(hello["payload"]["session"].as_str().unwrap().len(), 22);
  (sink, stream)
}

async fn next_json<S>(stream: &mut S) -> Option<Value>
where
  S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
  loop {
    match tokio::time::timeout(Duration::from_secs(5), stream.next())
      .await
      .ok()??
    {
      Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).ok(),
      Ok(Message::Close(_)) | Err(_) => return None,
      Ok(_) => {}
    }
  }
}

async fn send<S>(sink: &mut S, frame: Value)
where
  S: futures_util::Sink<Message> + Unpin,
  S::Error: std::fmt::Debug,
{
  sink
    .send(Message::Text(frame.to_string().into()))
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_frame_decode_rejects_unknown_kind() {
  let fx = serve(RpcWsConfig::default()).await;
  // Connection frames.
  let (mut sink, mut stream) = raw(fx.addr).await;
  send(&mut sink, json!({"id": 0, "kind": "ping"})).await;
  assert_eq!(next_json(&mut stream).await.unwrap()["kind"], "pong");
  send(&mut sink, json!({"id": 0, "kind": "pong"})).await;
  let session = "AAAAAAAAAAAAAAAAAAAAAA";
  let resume = json!({"id": 0, "kind": "resume", "payload": {"session": session, "calls": [
    {"id": 5, "cursor": Cursor::sequence(1).to_string()}
  ]}});
  send(&mut sink, resume).await;
  // A resume is answered with the connection's session (its own: this one is unknown).
  assert_eq!(next_json(&mut stream).await.unwrap()["kind"], "hello");
  let expired = next_json(&mut stream).await.unwrap();
  assert_eq!(expired["id"], 5);
  assert_eq!(expired["payload"]["error"]["_tag"], "ResumeExpired");
  // Frames for calls that are not open are ignored.
  send(&mut sink, json!({"id": 9, "kind": "item", "payload": 1})).await;
  for kind in ["end", "cancel"] {
    send(&mut sink, json!({"id": 9, "kind": kind})).await;
  }
  send(&mut sink, json!({"id": 9, "kind": "credit", "credits": 3})).await;
  send(
    &mut sink,
    json!({"id": 1, "kind": "call", "tag": "Greeter.greet", "payload": "x"}),
  )
  .await;
  assert_eq!(
    next_json(&mut stream).await.unwrap()["payload"]["value"],
    "Hello, x!"
  );
  // An unknown kind closes the socket.
  send(&mut sink, json!({"id": 1, "kind": "shout"})).await;
  assert!(next_json(&mut stream).await.is_none());

  // A malformed resume, a client-sent exit, a call id reused while open: each closes.
  for bad in [
    json!({"id": 0, "kind": "resume", "payload": {"session": 1}}),
    json!({"id": 1, "kind": "exit", "payload": {"_tag": "Success", "value": 1}}),
  ] {
    let (mut sink, mut stream) = raw(fx.addr).await;
    send(&mut sink, bad).await;
    assert!(next_json(&mut stream).await.is_none());
  }
  let (mut sink, mut stream) = raw(fx.addr).await;
  let open = json!({"id": 2, "kind": "call", "tag": "Feed.idle", "payload": "a"});
  send(&mut sink, open.clone()).await;
  send(&mut sink, open).await;
  assert!(next_json(&mut stream).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn credits_overrun_and_late_items() {
  let fx = serve(RpcWsConfig::default().window(2)).await;
  let (mut sink, mut stream) = raw(fx.addr).await;
  // A client stream fed past its credits fails with ResourceExhausted { credits }.
  send(
    &mut sink,
    json!({"id": 1, "kind": "call", "tag": "Sink.stall"}),
  )
  .await;
  for n in 0..6 {
    send(&mut sink, json!({"id": 1, "kind": "item", "payload": n})).await;
  }
  let mut exit = None;
  while let Some(frame) = next_json(&mut stream).await {
    if frame["kind"] == "exit" {
      exit = Some(frame);
      break;
    }
  }
  let exit = exit.unwrap();
  assert_eq!(exit["payload"]["error"]["_tag"], "ResourceExhausted");
  assert_eq!(exit["payload"]["error"]["limit"], "credits");
  // An item with END closes the input; credits are granted back as items are consumed.
  send(
    &mut sink,
    json!({"id": 3, "kind": "call", "tag": "Sum.all"}),
  )
  .await;
  send(&mut sink, json!({"id": 3, "kind": "item", "payload": 4})).await;
  send(
    &mut sink,
    json!({"id": 3, "kind": "item", "flags": 1, "payload": 5}),
  )
  .await;
  let mut credits = 0;
  loop {
    let frame = next_json(&mut stream).await.unwrap();
    if frame["kind"] == "credit" {
      credits += frame["credits"].as_u64().unwrap();
      continue;
    }
    assert_eq!(frame["payload"]["value"], 9);
    break;
  }
  assert!(credits >= 1);
  // A huge credit grant is clamped, not an error.
  send(
    &mut sink,
    json!({"id": 4, "kind": "call", "tag": "Count.up", "payload": 3}),
  )
  .await;
  send(
    &mut sink,
    json!({"id": 4, "kind": "credit", "credits": u32::MAX}),
  )
  .await;
  let mut seen = Vec::new();
  while let Some(frame) = next_json(&mut stream).await {
    seen.push(frame["kind"].as_str().unwrap().to_owned());
    if frame["kind"] == "exit" {
      break;
    }
  }
  assert_eq!(seen, ["item", "item", "item", "exit"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upgrades_without_an_rpc_subprotocol_are_refused() {
  let fx = serve(RpcWsConfig::default()).await;
  let request = url(fx.addr).into_client_request().unwrap();
  let err = tokio_tungstenite::connect_async(request).await.unwrap_err();
  let tokio_tungstenite::tungstenite::Error::Http(response) = err else {
    panic!("an http refusal: {err:?}");
  };
  assert_eq!(response.status(), 415);
  // A plain GET still gets the realtime layer's 426.
  let plain = reqwest_free_get(fx.addr).await;
  assert!(plain.starts_with("HTTP/1.1 426"), "{plain}");
  // A connect failure is a transport error.
  let closed = WsClient::connect("ws://127.0.0.1:1/rpc/ws")
    .await
    .unwrap_err();
  assert!(matches!(closed, RpcClientError::Transport(_)));
  assert!(WsClient::connect("not a url").await.is_err());
}

/// A bare `GET /rpc/ws` over TCP, the status line back.
async fn reqwest_free_get(addr: SocketAddr) -> String {
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
  let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
  tcp
    .write_all(b"GET /rpc/ws HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
    .await
    .unwrap();
  let mut out = Vec::new();
  tcp.read_to_end(&mut out).await.unwrap();
  String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_client_chooses_websocket_from_config() {
  let fx = serve(RpcWsConfig::default()).await;
  let client =
    RpcClient::new(format!("http://{}", fx.addr)).with_transport(TransportKind::WebSocket);
  let env = id_effect::hlist![id_effect_platform::http::HttpClientService(Arc::new(
    id_effect_platform::http::ReqwestHttpClient::default_client()
  )
    as Arc<dyn id_effect_platform::http::HttpClient>)];
  let open = client
    .subscribe("Feed.watch", json!("z"), None)
    .provide_all(env.clone());
  let stream = match id_effect_axum::EffectExecutor::global()
    .run_effect(open)
    .await
  {
    id_effect::Exit::Success(stream) => stream,
    id_effect::Exit::Failure(cause) => panic!("{cause:?}"),
  };
  assert!(format!("{stream:?}").contains("RpcStream"));
  let mut got = Vec::new();
  loop {
    let next = id_effect_axum::EffectExecutor::global()
      .run_effect(stream.next())
      .await;
    match next {
      id_effect::Exit::Success(Some(CallEvent::Item { value, .. })) => got.push(value),
      id_effect::Exit::Success(Some(CallEvent::Exit(result))) => {
        assert!(result.is_ok());
      }
      id_effect::Exit::Success(None) => break,
      id_effect::Exit::Failure(cause) => panic!("{cause:?}"),
    }
  }
  assert_eq!(got, [json!("z#0"), json!("z#1"), json!("z#2")]);
  // HTTP carries no subscriptions.
  let http = RpcClient::new(format!("http://{}", fx.addr));
  let refused = http
    .subscribe("Feed.watch", json!("z"), None)
    .provide_all(env);
  let id_effect::Exit::Failure(cause) = id_effect_axum::EffectExecutor::global()
    .run_effect(refused)
    .await
  else {
    panic!("refused");
  };
  assert!(matches!(
    cause.fail_option(),
    Some(RpcClientError::Transport(_))
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streams_end_overrun_and_oversize_on_their_own() {
  let fx = serve(RpcWsConfig::default()).await;
  let client = WsClient::connect(&url(fx.addr)).await.unwrap();
  // A two-way stream that ignores its input still streams after the client ended it.
  let mut ignore = client.open("Echo.ignore", Value::Null, None).await.unwrap();
  ignore.end().await.unwrap();
  let (got, exit) = items(&mut ignore).await;
  assert_eq!(got.len(), 50);
  assert!(exit.is_ok());
  // An item over the frame cap fails the call after the items before it.
  let mut big = client.open("Big.blob", json!(70_000), None).await.unwrap();
  let (got, exit) = items(&mut big).await;
  assert_eq!(got, [json!("small")]);
  assert_eq!(
    exit.unwrap_err().reserved(),
    Some(id_effect_rpc::wire::RpcFailure::ResourceExhausted {
      limit: "max_frame".into()
    })
  );
  // A two-way stream fed past its credits fails with ResourceExhausted { credits }.
  let (mut sink, mut stream) = raw(fx.addr).await;
  send(
    &mut sink,
    json!({"id": 1, "kind": "call", "tag": "Echo.hold"}),
  )
  .await;
  // The handler's own queue (the stream window, 32) and the call's (32 credits) fill first.
  for n in 0..80 {
    send(&mut sink, json!({"id": 1, "kind": "item", "payload": n})).await;
  }
  let mut exit = None;
  while let Some(frame) = next_json(&mut stream).await {
    if frame["kind"] == "exit" {
      exit = Some(frame);
      break;
    }
  }
  assert_eq!(exit.unwrap()["payload"]["error"]["limit"], "credits");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_vanishes_ends_its_calls() {
  let fx = serve(RpcWsConfig::default()).await;
  let client = WsClient::connect(&url(fx.addr)).await.unwrap();
  let mut call = client.open("Count.forever", json!(0), None).await.unwrap();
  for _ in 0..40 {
    assert!(matches!(call.next().await, Some(CallEvent::Item { .. })));
  }
  drop(call);
  drop(client);
  // The socket closed: the stream's producer was interrupted and stops.
  tokio::time::sleep(Duration::from_millis(200)).await;
  let after = fx.produced.load(Ordering::SeqCst);
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(fx.produced.load(Ordering::SeqCst), after);
  // A raw client that goes away mid-stream, without a close frame.
  let (mut sink, stream) = raw(fx.addr).await;
  send(
    &mut sink,
    json!({"id": 1, "kind": "call", "tag": "Count.forever", "payload": 0}),
  )
  .await;
  tokio::time::sleep(Duration::from_millis(50)).await;
  drop((sink, stream));
  tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_socket_runs_on_any_upgraded_io() {
  use id_effect_axum::realtime::ws::{WsConfig, serve_io};
  use tokio_tungstenite::tungstenite::protocol::Role;
  let server = RpcServer::new(group(Arc::new(AtomicUsize::new(0))));
  let config = RpcWsConfig::default();
  let (ours, theirs) = tokio::io::duplex(256 * 1024);
  let head = axum::http::Request::new(()).into_parts().0;
  let session = serve_io(ours, &WsConfig::default(), move |socket| {
    id_effect_rpc::transport::ws::serve_socket(
      &server,
      &config,
      DynEnv::new(),
      head,
      id_effect_rpc::codec::CodecId::Json,
      socket,
    )
  });
  tokio::spawn(id_effect_axum::EffectExecutor::global().run_effect(session));
  let socket =
    tokio_tungstenite::WebSocketStream::from_raw_socket(theirs, Role::Client, None).await;
  let client = WsClient::over(socket).await.unwrap();
  assert_eq!(
    client.call("Greeter.greet", json!("io")).await.unwrap(),
    "Hello, io!"
  );
}

/// A fake server: accepts one socket with the rpc subprotocol and plays `script`.
#[allow(clippy::result_large_err)]
async fn fake(script: Vec<Message>) -> SocketAddr {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    let (tcp, _) = listener.accept().await.unwrap();
    let callback =
      |_: &tokio_tungstenite::tungstenite::handshake::server::Request,
       mut res: tokio_tungstenite::tungstenite::handshake::server::Response| {
        res
          .headers_mut()
          .insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
        Ok(res)
      };
    let mut ws = tokio_tungstenite::accept_hdr_async(tcp, callback)
      .await
      .unwrap();
    for msg in script {
      let _ = ws.send(msg).await;
      tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
  });
  addr
}

fn hello() -> Message {
  let hello = json!({"id": 0, "kind": "hello", "payload": {
    "session": "AAAAAAAAAAAAAAAAAAAAAA", "codec": "idrpc.json.v1", "max_frame": 65536, "window": 2
  }});
  Message::Text(hello.to_string().into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ws_client_survives_a_misbehaving_server() {
  // No hello.
  let addr = fake(vec![Message::Text("{}".into())]).await;
  let err = WsClient::connect(&url(addr)).await.unwrap_err();
  assert!(err.to_string().contains("hello"), "{err}");

  // Credits, stray frames, pongs, pings and binary frames are routed or skipped; a frame that
  // does not decode closes the socket, and open calls then end with a transport error.
  let text = |v: Value| Message::Text(v.to_string().into());
  let binary = json!({"id": 1, "kind": "item", "payload": 8}).to_string();
  let addr = fake(vec![
    hello(),
    text(json!({"id": 1, "kind": "credit", "credits": 5})),
    text(json!({"id": 1, "kind": "item", "payload": 7})),
    text(json!({"id": 9, "kind": "item", "payload": 7})),
    text(json!({"id": 1, "kind": "end", "flags": 1})),
    Message::Ping(Vec::new().into()),
    Message::Binary(binary.into_bytes().into()),
    Message::Text("not a frame".into()),
  ])
  .await;
  let client = WsClient::connect(&url(addr)).await.unwrap();
  let mut call = client.open("A.b", Value::Null, None).await.unwrap();
  assert_eq!(call.id().get(), 1);
  for _ in 0..4 {
    call.send(json!(1)).await.unwrap();
  }
  assert!(matches!(call.next().await, Some(CallEvent::Item { value, .. }) if value == 7));
  assert!(matches!(call.next().await, Some(CallEvent::Item { value, .. }) if value == 8));
  assert!(matches!(
    call.next().await,
    Some(CallEvent::Exit(Err(RpcClientError::Transport(_))))
  ));
  assert!(call.next().await.is_none());
  assert!(client.open("A.b", Value::Null, None).await.is_err());
  // A binary first message is no hello either.
  let addr = fake(vec![Message::Binary(vec![1].into())]).await;
  assert!(WsClient::connect(&url(addr)).await.is_err());
  // A server that ignores its credits (window 2) breaks the protocol: the socket is dropped, and
  // `finish` skips the items it got and ends with a transport error.
  let flood = (0..4).map(|n| text(json!({"id": 1, "kind": "item", "payload": n})));
  let addr = fake(std::iter::once(hello()).chain(flood).collect()).await;
  let flooded = WsClient::connect(&url(addr)).await.unwrap();
  let call = flooded.open("A.b", Value::Null, None).await.unwrap();
  assert!(matches!(
    call.finish().await,
    Err(RpcClientError::Transport(_))
  ));
  // A server that closes: a call still waiting for its exit ends with a transport error.
  let addr = fake(vec![hello(), Message::Close(None)]).await;
  let finished = WsClient::connect(&url(addr)).await.unwrap();
  let call = finished.open("A.b", Value::Null, None).await.unwrap();
  assert!(call.finish().await.is_err());
}

// ---------------------------------------------------------------------------------------------
// A generated client (`#[rpc::service]`) over the WebSocket transport
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, id_effect_rpc::RpcErrorUnion)]
enum CountError {
  #[rpc(status = 404)]
  Negative,
}

/// Counts.
#[id_effect_rpc::service(version = 1)]
trait Counter {
  /// Twice `n`.
  #[query]
  #[public]
  fn twice(&self, n: i64) -> id_effect::Effect<i64, CountError, DynEnv>;

  /// 1 to `n`.
  #[server_stream]
  #[public]
  fn upto(&self, n: i64) -> id_effect::Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;

  /// The sum of the items.
  #[client_stream]
  #[public]
  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> id_effect::Effect<i64, Never, DynEnv>;

  /// Each item doubled.
  #[bidi]
  #[public]
  fn double(&self) -> id_effect::Effect<Duplex<i64, i64, Never>, Never, DynEnv>;

  /// Three ticks from `from`, resumable.
  #[subscription]
  #[public]
  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> id_effect::Effect<Subscription<i64, Never>, Never, DynEnv>;
}

struct Count;

impl Counter for Count {
  fn twice(&self, n: i64) -> id_effect::Effect<i64, CountError, DynEnv> {
    if n < 0 {
      id_effect::fail(CountError::Negative)
    } else {
      succeed(n * 2)
    }
  }

  fn upto(&self, n: i64) -> id_effect::Effect<Stream<i64, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::from_iterable(1..=n))
  }

  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> id_effect::Effect<i64, Never, DynEnv> {
    items.run_fold(0, |a, b| a + b)
  }

  fn double(&self) -> id_effect::Effect<Duplex<i64, i64, Never>, Never, DynEnv> {
    succeed(Duplex::new(|items: Stream<i64, Never, DynEnv>| {
      items.map(|n| n * 2)
    }))
  }

  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> id_effect::Effect<Subscription<i64, Never>, Never, DynEnv> {
    let start = after.map_or(from, |c| c.position as i64 + 1);
    let items = (start..from + 3)
      .map(|n| SubscriptionItem::new(n).with_cursor(Cursor::events(n as u64)))
      .collect();
    succeed(Subscription::new(Stream::from_vec(items)))
  }
}

async fn run<A, E>(effect: id_effect::Effect<A, E, id_effect::HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  id_effect_axum::run_with_env(id_effect::HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| cause.fail_option().expect("a typed failure"))
}

async fn listen(server: RpcServer) -> SocketAddr {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_clients_call_every_kind_over_websocket() {
  let socket = id_effect_axum::realtime::ws::WsConfig::default().messages_per_second(10_000);
  let server = RpcServer::new(CounterService::group(Count))
    .with_websocket(RpcWsConfig::default().socket(socket));
  let addr = listen(server).await;
  let ws = WsClient::connect(&url(addr)).await.unwrap();
  let client = CounterService::client(ws);

  assert_eq!(run(client.twice(21)).await.unwrap(), 42);
  let negative = run(client.twice(-1)).await.unwrap_err();
  assert_eq!(negative.typed(), Some(&CountError::Negative));
  // More items than the credit window, both ways.
  let upto = run(client.upto(100).flat_map(|items| items.run_collect()))
    .await
    .unwrap();
  assert_eq!(upto, (1..=100).collect::<Vec<_>>());
  assert_eq!(run(client.sum((1..=100).collect())).await.unwrap(), 5050);
  let doubled = client.double().flat_map(|(tx, out)| {
    let tx = Arc::new(tx);
    let sends = (1..=40).fold(succeed::<(), _, _>(()), |acc, n| {
      let tx = tx.clone();
      acc.flat_map(move |()| tx.send(n))
    });
    sends
      .map(move |()| drop(tx))
      .flat_map(move |()| out.run_collect())
  });
  let doubled = run(doubled).await.unwrap();
  assert_eq!(doubled, (1..=40).map(|n| n * 2).collect::<Vec<_>>());
  let ticks = run(client.ticks(5, None).flat_map(|s| s.run_collect()))
    .await
    .unwrap();
  assert_eq!(ticks.iter().map(|t| t.value).collect::<Vec<_>>(), [5, 6, 7]);
  let resumed = run(
    client
      .ticks(5, ticks[0].cursor)
      .flat_map(|s| s.run_collect()),
  )
  .await
  .unwrap();
  assert_eq!(resumed.iter().map(|t| t.value).collect::<Vec<_>>(), [6, 7]);

  // A server without the service: unary calls are refused, streams end with the refusal.
  let elsewhere = listen(
    RpcServer::new(group(Arc::new(AtomicUsize::new(0)))).with_websocket(RpcWsConfig::default()),
  )
  .await;
  let stranger = CounterService::client(WsClient::connect(&url(elsewhere)).await.unwrap());
  let refused = run(stranger.twice(1)).await.unwrap_err();
  assert!(refused.typed().is_none(), "{refused:?}");
  assert!(
    run(stranger.upto(1).flat_map(|s| s.run_collect()))
      .await
      .is_err()
  );
  assert!(run(stranger.sum(vec![1])).await.is_err());
  // A closed socket: the transport fails, streams end as cancelled.
  let gone = fake(vec![hello(), Message::Close(None)]).await;
  let lost = CounterService::client(WsClient::connect(&url(gone)).await.unwrap());
  assert!(
    run(lost.upto(3).flat_map(|s| s.run_collect()))
      .await
      .is_err()
  );
  assert!(run(lost.twice(1)).await.is_err());
}

/// Channel joins on one socket share its connection and see the principal its upgrade
/// request authenticated.
#[cfg(feature = "channels")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channel_joins_on_one_socket_share_one_conn() {
  use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Decision};
  use id_effect_node::{BehaviorRegistry, Mesh, NodeId};
  use id_effect_process::{NodeName, ProcessNode};
  use id_effect_rpc::channels::{Channels, JoinRequest, join_policy_fn, register_hub};

  let mut registry = BehaviorRegistry::new();
  register_hub(&mut registry);
  let node = NodeName::new("ws@test");
  let mesh = Mesh::new(NodeId::fresh(&node), ProcessNode::new(node), registry);
  let channels = Channels::<String>::builder()
    .join_policy(join_policy_fn(|join: &JoinRequest| {
      Decision::allow_if(join.principal.has_role("member"), "members only")
    }))
    .start_on_mesh(&mesh)
    .expect("hub");
  let mut joins = RpcGroup::new();
  channels.serve_joins(&mut joins, Procedure::subscription("Chat.join").public());
  let server = RpcServer::new(joins).with_websocket(RpcWsConfig::default());
  // What an `Authenticate` layer does: the principal as a request capability.
  let app = server
    .router()
    .with_state(DynEnv::new())
    .into_axum()
    .layer(axum::middleware::from_fn(
      |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
        let mut caps = id_effect_axum::RequestCaps::new();
        caps.insert(CurrentPrincipal::new(
          BasicPrincipal::new("ada").with_role("member"),
        ));
        req.extensions_mut().insert(caps);
        next.run(req).await
      },
    ));
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
  let me = async |client: &WsClient, topic: &str| {
    let mut call = client
      .open("Chat.join", json!({"topic": topic}), None)
      .await
      .unwrap();
    let Some(CallEvent::Item { value, .. }) = call.next().await else {
      panic!("joined");
    };
    assert_eq!(value["type"], "joined", "{value}");
    (value["me"]["conn"].as_str().unwrap().to_owned(), call)
  };
  let socket = WsClient::connect(&url(addr)).await.unwrap();
  let (a, _keep_a) = me(&socket, "room:a").await;
  let (b, _keep_b) = me(&socket, "room:b").await;
  assert_eq!(a, b, "one socket, one connection");
  let other = WsClient::connect(&url(addr)).await.unwrap();
  let (c, _keep_c) = me(&other, "room:a").await;
  assert_ne!(a, c, "another socket is another connection");
}

#[cfg(feature = "postcard")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postcard_frames_over_websocket() {
  use id_effect_rpc::codec::Codec as _;
  use id_effect_rpc::codec::postcard::{Postcard, decode_exit, decode_frame, encode_frame};
  use id_effect_rpc::wire::{CallId, Frame, FrameKind, Hello, WireExit};

  let fx = serve(RpcWsConfig::default()).await;
  let mut request = url(fx.addr).into_client_request().unwrap();
  request.headers_mut().insert(
    "sec-websocket-protocol",
    "idrpc.postcard.v1".parse().unwrap(),
  );
  let (socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
  assert_eq!(
    response.headers()["sec-websocket-protocol"],
    "idrpc.postcard.v1"
  );
  let (mut sink, mut stream) = socket.split();
  let mut next = async || loop {
    match stream.next().await.unwrap().unwrap() {
      Message::Binary(bytes) => return decode_frame(&bytes).unwrap(),
      Message::Close(_) => panic!("closed"),
      _ => {}
    }
  };
  let hello = next().await;
  assert_eq!(hello.kind, FrameKind::Hello);
  let hello: Hello = Postcard::decode(&hello.payload.unwrap()).unwrap();
  assert_eq!(hello.codec, "idrpc.postcard.v1");
  let id = |n| CallId::new(n).unwrap();
  let call = |n, tag: &str, payload: Vec<u8>| {
    Message::Binary(
      encode_frame(&Frame::call(id(n), tag, payload))
        .unwrap()
        .into(),
    )
  };
  let exit_of = |frame: Frame<Vec<u8>>| decode_exit(&frame.payload.unwrap()).unwrap();

  // A typed procedure: the payload and the output cross as postcard.
  sink
    .send(call(1, "Typed.add", Postcard::encode(&41_i64).unwrap()))
    .await
    .unwrap();
  let WireExit::Success { value } = exit_of(next().await) else {
    panic!("a success");
  };
  assert_eq!(Postcard::decode::<i64>(&value).unwrap(), 42);
  // Bytes that are not the input type: InvalidInput.
  sink
    .send(call(2, "Typed.add", vec![0xff; 12]))
    .await
    .unwrap();
  let WireExit::Failure { error } = exit_of(next().await) else {
    panic!("a failure");
  };
  assert_eq!(error["_tag"], "InvalidInput");
  // A procedure without a typed bridge refuses postcard: Rejected 415.
  sink
    .send(call(3, "Greeter.greet", vec![1, b'a']))
    .await
    .unwrap();
  let WireExit::Failure { error } = exit_of(next().await) else {
    panic!("a failure");
  };
  assert_eq!(
    (error["_tag"].as_str(), error["status"].as_u64()),
    (Some("Rejected"), Some(415))
  );
  // A call without input reaches the handler (`null`), whose typed failure needs no bridge.
  let mut bare = Frame::<Vec<u8>>::new(id(4), FrameKind::Call);
  bare.tag = Some("Greeter.greet".into());
  sink
    .send(Message::Binary(encode_frame(&bare).unwrap().into()))
    .await
    .unwrap();
  let WireExit::Failure { error } = exit_of(next().await) else {
    panic!("a failure");
  };
  assert!(
    matches!(
      error["_tag"].as_str(),
      Some("InvalidInput" | "InternalError")
    ),
    "{error}"
  );
  // Client stream items cross the bridge too; bytes that are not the item type fail the call.
  let item = |n, payload: Vec<u8>| {
    Message::Binary(
      encode_frame(&Frame::new(id(n), FrameKind::Item).with_payload(payload))
        .unwrap()
        .into(),
    )
  };
  let open = |n| {
    let mut f = Frame::<Vec<u8>>::new(id(n), FrameKind::Call);
    f.tag = Some("Typed.sum".into());
    Message::Binary(encode_frame(&f).unwrap().into())
  };
  sink.send(open(5)).await.unwrap();
  for n in [2_i64, 3] {
    sink
      .send(item(5, Postcard::encode(&n).unwrap()))
      .await
      .unwrap();
  }
  let end = Frame::<Vec<u8>>::new(id(5), FrameKind::End);
  sink
    .send(Message::Binary(encode_frame(&end).unwrap().into()))
    .await
    .unwrap();
  let mut credits_or_exit = next().await;
  while credits_or_exit.kind == FrameKind::Credit {
    credits_or_exit = next().await;
  }
  let WireExit::Success { value } = exit_of(credits_or_exit) else {
    panic!("a success");
  };
  assert_eq!(Postcard::decode::<i64>(&value).unwrap(), 5);
  sink.send(open(6)).await.unwrap();
  sink.send(item(6, vec![0xff; 12])).await.unwrap();
  let mut credits_or_exit = next().await;
  while credits_or_exit.kind == FrameKind::Credit {
    credits_or_exit = next().await;
  }
  let WireExit::Failure { error } = exit_of(credits_or_exit) else {
    panic!("a failure");
  };
  assert_eq!(error["_tag"], "InvalidInput");
  // A resume in postcard.
  let resume = id_effect_rpc::wire::Resume {
    session: id_effect_rpc::wire::SessionId::random(),
    calls: vec![id_effect_rpc::wire::ResumeCall {
      id: id(9),
      cursor: Cursor::sequence(1),
    }],
  };
  let resume = Frame::new(CallId::CONNECTION, FrameKind::Resume)
    .with_payload(Postcard::encode(&resume).unwrap());
  sink
    .send(Message::Binary(encode_frame(&resume).unwrap().into()))
    .await
    .unwrap();
  assert_eq!(next().await.kind, FrameKind::Hello);
  let expired = next().await;
  assert_eq!(expired.id, id(9));
  let WireExit::Failure { error } = exit_of(expired) else {
    panic!("a failure");
  };
  assert_eq!(error["_tag"], "ResumeExpired");
  // An output (or item) the codec cannot carry: InternalError.
  for (n, tag) in [(7, "Half.query"), (8, "Half.stream")] {
    sink
      .send(call(n, tag, Postcard::encode(&1_i64).unwrap()))
      .await
      .unwrap();
    let WireExit::Failure { error } = exit_of(next().await) else {
      panic!("a failure");
    };
    assert_eq!(error["_tag"], "InternalError", "{tag}");
  }
  // A frame that does not decode closes the socket.
  sink
    .send(Message::Binary(vec![0xff; 3].into()))
    .await
    .unwrap();
  assert!(matches!(
    stream.next().await,
    None | Some(Ok(Message::Close(_))) | Some(Err(_))
  ));
}
