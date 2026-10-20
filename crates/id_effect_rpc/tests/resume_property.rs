//! Resume after a reconnect (design note §6, r12-e12-2-bidi-resume): the property that a client
//! which drops its socket at random points and resumes from its last cursor sees every item
//! exactly once and in order (within the buffer bounds), and the refusals around it — an overrun
//! buffer, another principal, guards that no longer admit, a session left past its retention.
//!
//! Gate: `PROPTEST_CASES=1000 cargo nextest run -p id_effect_rpc --all-features --test
//! resume_property`. Real sockets on loopback, raw frames on the client side.
#![cfg(feature = "ws")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::request::Parts;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use id_effect::{DynEnv, Never, Stream, schema, succeed};
use id_effect_axum::realtime::ws::WsConfig;
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_rpc::resume::{ResumeConfig, TestClock};
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::wire::Cursor;
use id_effect_rpc::{Duplex, Procedure, RpcGroup, RpcServer, WireSchema};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
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

/// Sets its flag when dropped: the handler's stream was released.
struct Released(Arc<AtomicBool>);

impl Drop for Released {
  fn drop(&mut self) {
    self.0.store(true, Ordering::SeqCst);
  }
}

fn group(released: Arc<AtomicBool>) -> RpcGroup {
  let mut group = RpcGroup::new();
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.server_stream(
    Procedure::server_stream("Secret.count").guard(Bearer),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.server_stream(
    Procedure::server_stream("Count.forever").public(),
    int(),
    int(),
    move |_: i64, _: &mut DynEnv| {
      let guard = Released(released.clone());
      succeed::<_, Never, _>(Stream::unfold(1_i64, move |n| {
        let _held = &guard;
        Some((n, n + 1))
      }))
    },
  );
  // Ignores its input: 100 items whatever the client sends.
  group.bidi(
    Procedure::bidi("Echo.hundred").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|_items| Stream::from_iterable(1..=100))),
  );
  group
}

fn config() -> RpcWsConfig {
  let socket = WsConfig::default()
    .messages_per_second(100_000)
    .max_sockets(100_000, 100_000);
  RpcWsConfig::default().socket(socket)
}

async fn serve(config: RpcWsConfig, released: Arc<AtomicBool>) -> SocketAddr {
  let server = RpcServer::new(group(released))
    .with_principal(|parts| {
      parts
        .headers
        .get("x-user")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
    })
    .with_websocket(config);
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

/// One raw client connection, past its `hello`.
struct Client {
  sink: SplitSink<Socket, Message>,
  stream: SplitStream<Socket>,
  session: String,
}

impl Client {
  async fn connect(addr: SocketAddr, headers: &[(&'static str, &'static str)]) -> Self {
    let mut request = format!("ws://{addr}/rpc/ws").into_client_request().unwrap();
    request
      .headers_mut()
      .insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
    for (name, value) in headers {
      request.headers_mut().insert(*name, value.parse().unwrap());
    }
    let (socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let (sink, stream) = socket.split();
    let mut client = Self {
      sink,
      stream,
      session: String::new(),
    };
    let hello = client.next().await.expect("hello");
    assert_eq!(hello["kind"], "hello");
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

  /// The next frame, or `None` if none comes within `wait`.
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

  /// Send `resume` for `calls` (`(id, cursor)`) of `session`; returns the session the answer's
  /// `hello` names.
  async fn resume(&mut self, session: &str, calls: &[(u64, Cursor)]) -> String {
    let calls: Vec<Value> = calls
      .iter()
      .map(|(id, cursor)| json!({"id": id, "cursor": cursor.to_string()}))
      .collect();
    self
      .send(json!({"id": 0, "kind": "resume", "payload": {"session": session, "calls": calls}}))
      .await;
    let hello = self.next().await.expect("hello after resume");
    assert_eq!(hello["kind"], "hello");
    hello["payload"]["session"].as_str().unwrap().to_owned()
  }
}

fn cursor_of(frame: &Value) -> Cursor {
  frame["cursor"]
    .as_str()
    .expect("server items of resumable calls carry a cursor")
    .parse()
    .unwrap()
}

/// What one property case does: the procedure, the disconnect points (items received before
/// each drop) and the credit batch.
#[derive(Debug, Clone)]
struct Case {
  bidi: bool,
  n: usize,
  cuts: Vec<usize>,
  batch: u64,
}

/// Run `case`: call, read items, drop the socket at each cut, resume from the last cursor,
/// until the exit. Returns the items seen.
async fn run_case(addr: SocketAddr, case: &Case) -> Vec<i64> {
  let mut client = Client::connect(addr, &[]).await;
  let session = client.session.clone();
  if case.bidi {
    client
      .send(json!({"id": 1, "kind": "call", "tag": "Echo.hundred", "payload": null}))
      .await;
  } else {
    client
      .send(json!({"id": 1, "kind": "call", "tag": "Count.up", "payload": case.n}))
      .await;
  }
  let mut cuts = case.cuts.clone();
  cuts.sort_unstable();
  let mut cuts = cuts.into_iter().peekable();
  let mut got = Vec::new();
  let mut cursor = Cursor::sequence(0);
  let mut pending = 0_u64;
  loop {
    while cuts.next_if(|cut| *cut <= got.len()).is_some() {
      drop(client);
      client = Client::connect(addr, &[]).await;
      let now = client.resume(&session, &[(1, cursor)]).await;
      assert_eq!(now, session, "the resumed session carries on");
      // The items up to the cursor count as credited: nothing left to grant for them.
      pending = 0;
    }
    let frame = client.next().await.expect("a frame before the exit");
    match frame["kind"].as_str() {
      Some("item") => {
        assert_eq!(frame["id"], 1);
        cursor = cursor_of(&frame);
        got.push(frame["payload"].as_i64().unwrap());
        pending += 1;
        if pending >= case.batch {
          client
            .send(json!({"id": 1, "kind": "credit", "credits": pending}))
            .await;
          pending = 0;
        }
      }
      Some("exit") => {
        assert_eq!(frame["payload"]["_tag"], "Success", "{frame}");
        return got;
      }
      // A two-way call's input window.
      Some("credit") => {}
      other => panic!("unexpected frame {other:?}: {frame}"),
    }
  }
}

fn case() -> impl Strategy<Value = Case> {
  (
    any::<bool>(),
    1_usize..=200,
    proptest::collection::vec(0_usize..=200, 0..=4),
    1_u64..=16,
  )
    .prop_map(|(bidi, n, cuts, batch)| {
      let n = if bidi { 100 } else { n };
      Case {
        bidi,
        n,
        cuts: cuts.into_iter().filter(|cut| *cut <= n).collect(),
        batch,
      }
    })
}

#[test]
fn resume_after_disconnect_no_gaps_no_duplicates() {
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(4)
    .enable_all()
    .build()
    .unwrap();
  let addr = runtime.block_on(serve(config(), Arc::default()));
  // `Config::default` reads `PROPTEST_CASES` (the gate runs 1,000).
  let mut runner = TestRunner::new(Config {
    failure_persistence: None,
    ..Config::default()
  });
  runner
    .run(&case(), |case| {
      let got = runtime.block_on(run_case(addr, &case));
      let want: Vec<i64> = (1..=case.n as i64).collect();
      prop_assert_eq!(got, want, "{:?}", case);
      Ok(())
    })
    .unwrap();
}

/// Read `n` items of call 1; returns the last cursor.
async fn read_items(client: &mut Client, n: usize) -> Cursor {
  let mut cursor = Cursor::sequence(0);
  for _ in 0..n {
    let frame = client.next().await.unwrap();
    assert_eq!(frame["kind"], "item", "{frame}");
    cursor = cursor_of(&frame);
  }
  cursor
}

async fn until(mut done: impl FnMut() -> bool) {
  for _ in 0..500 {
    if done() {
      return;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert!(done(), "condition not reached in 5 s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_buffer_overrun_is_expired() {
  let config = config().resume(ResumeConfig::new().max_messages(8));
  let addr = serve(config.clone(), Arc::default()).await;
  let mut client = Client::connect(addr, &[]).await;
  let session = client.session.clone();
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Count.up", "payload": 100}))
    .await;
  // No credit: the server sends the window (32) and stops; the buffer keeps the newest 8.
  let cursor = read_items(&mut client, 2).await;
  until(|| config.resume_stats().messages == 8).await;
  drop(client);
  let mut client = Client::connect(addr, &[]).await;
  // Call 3 was never opened in the session.
  let calls = [(1, cursor), (3, Cursor::sequence(0))];
  assert_eq!(client.resume(&session, &calls).await, session);
  for id in [1, 3] {
    let exit = client.next().await.unwrap();
    assert_eq!(exit["kind"], "exit");
    assert_eq!(exit["id"], id);
    assert_eq!(exit["payload"]["error"]["_tag"], "ResumeExpired");
  }
  assert_eq!(config.resume_stats().expired, 2);
  assert_eq!(config.resume_config().messages_limit(), 8);
  // A call the session does not hold expires too.
  client.resume(&session, &[(4, Cursor::sequence(0))]).await;
  let exit = client.next().await.unwrap();
  assert_eq!(exit["payload"]["error"]["_tag"], "ResumeExpired");
  // The restarted stream runs.
  client
    .send(json!({"id": 2, "kind": "call", "tag": "Count.up", "payload": 1}))
    .await;
  assert_eq!(client.next().await.unwrap()["payload"], 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_under_another_principal_refused() {
  let config = config();
  let addr = serve(config.clone(), Arc::default()).await;
  let mut ada = Client::connect(addr, &[("x-user", "ada")]).await;
  let session = ada.session.clone();
  ada
    .send(json!({"id": 1, "kind": "call", "tag": "Count.up", "payload": 40}))
    .await;
  let cursor = read_items(&mut ada, 3).await;
  drop(ada);

  // Eve has the session id and a cursor: neither is a capability.
  for headers in [&[("x-user", "eve")][..], &[][..]] {
    let mut eve = Client::connect(addr, headers).await;
    let own = eve.session.clone();
    assert_eq!(eve.resume(&session, &[(1, cursor)]).await, own);
    let exit = eve.next().await.unwrap();
    assert_eq!(exit["payload"]["error"]["_tag"], "ResumeExpired");
  }
  assert_eq!(config.resume_stats().refused_principal, 2);

  // Ada's session is untouched: she resumes and gets the rest.
  let mut ada = Client::connect(addr, &[("x-user", "ada")]).await;
  assert_eq!(ada.resume(&session, &[(1, cursor)]).await, session);
  let mut rest = Vec::new();
  loop {
    let frame = ada.next().await.unwrap();
    match frame["kind"].as_str() {
      Some("item") => {
        rest.push(frame["payload"].as_i64().unwrap());
        ada
          .send(json!({"id": 1, "kind": "credit", "credits": 1}))
          .await;
      }
      _ => break,
    }
  }
  assert_eq!(rest, (4..=40).collect::<Vec<_>>());

  // A resume after a call was opened does not adopt the session.
  let mut late = Client::connect(addr, &[("x-user", "ada")]).await;
  late
    .send(json!({"id": 7, "kind": "call", "tag": "Count.up", "payload": 1}))
    .await;
  assert_eq!(late.next().await.unwrap()["kind"], "item");
  assert_eq!(late.next().await.unwrap()["kind"], "exit");
  let own = late.session.clone();
  assert_eq!(late.resume(&session, &[(1, cursor)]).await, own);
  let exit = late.next().await.unwrap();
  assert_eq!(exit["id"], 1);
  assert_eq!(exit["payload"]["error"]["_tag"], "ResumeExpired");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_resume_leaves_the_calls_open_on_its_connection_alone() {
  let addr = serve(config(), Arc::default()).await;
  let mut client = Client::connect(addr, &[]).await;
  client
    .send(json!({"id": 5, "kind": "call", "tag": "Count.forever", "payload": 0}))
    .await;
  read_items(&mut client, 32).await;
  let own = client.session.clone();
  // Not fresh any more: the resume adopts nothing, and call 5 is not its to answer.
  let stranger = Client::connect(addr, &[]).await.session;
  assert_eq!(
    client.resume(&stranger, &[(5, Cursor::sequence(0))]).await,
    own
  );
  assert!(
    client
      .next_within(Duration::from_millis(100))
      .await
      .is_none()
  );
  client
    .send(json!({"id": 5, "kind": "credit", "credits": 1}))
    .await;
  assert_eq!(client.next().await.unwrap()["payload"], 33);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_that_ended_while_detached_replays_its_exit() {
  let addr = serve(config(), Arc::default()).await;
  let mut client = Client::connect(addr, &[]).await;
  let session = client.session.clone();
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Count.up", "payload": 5}))
    .await;
  let cursor = read_items(&mut client, 2).await;
  drop(client);
  let mut client = Client::connect(addr, &[]).await;
  assert_eq!(client.resume(&session, &[(1, cursor)]).await, session);
  let mut rest = Vec::new();
  let exit = loop {
    let frame = client.next().await.unwrap();
    if frame["kind"] == "exit" {
      break frame;
    }
    rest.push(frame["payload"].as_i64().unwrap());
  };
  assert_eq!(rest, vec![3, 4, 5]);
  assert_eq!(exit["payload"]["_tag"], "Success");
  // The cursor of the last item still names the call's end: the exit again, nothing else.
  drop(client);
  let mut client = Client::connect(addr, &[]).await;
  let last = Cursor::sequence(cursor.position + 3);
  assert_eq!(client.resume(&session, &[(1, last)]).await, session);
  assert_eq!(client.next().await.unwrap()["kind"], "exit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumes_respect_the_call_cap() {
  let config = config().max_calls(2);
  let addr = serve(config.clone(), Arc::default()).await;
  let mut client = Client::connect(addr, &[]).await;
  let session = client.session.clone();
  // Call 1 ends (its frames stay in the session); then two open streams fill the cap.
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Count.up", "payload": 1}))
    .await;
  let one = read_items(&mut client, 1).await;
  assert_eq!(client.next().await.unwrap()["kind"], "exit");
  tokio::time::sleep(Duration::from_millis(100)).await;
  for id in [2, 3] {
    client
      .send(json!({"id": id, "kind": "call", "tag": "Count.forever", "payload": 0}))
      .await;
  }
  // Call 1's item and exit, and a window of each stream.
  until(|| config.resume_stats().messages == 2 + 2 * 32).await;
  assert_eq!(config.resume_stats().calls, 3);
  drop(client);
  // Three calls to resume, two slots: the third is refused with the cap's failure.
  let mut client = Client::connect(addr, &[]).await;
  let calls = [(1, one), (2, Cursor::sequence(0)), (3, Cursor::sequence(0))];
  assert_eq!(client.resume(&session, &calls).await, session);
  let mut refused = None;
  for _ in 0..100 {
    let frame = client.next().await.unwrap();
    if frame["id"] == 3 {
      refused = Some(frame);
      break;
    }
  }
  let refused = refused.expect("call 3 answered");
  assert_eq!(refused["kind"], "exit", "{refused}");
  assert_eq!(refused["payload"]["error"]["_tag"], "ResourceExhausted");
  assert_eq!(refused["payload"]["error"]["limit"], "calls");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resumed_calls_run_their_guards_again() {
  let config = config();
  let addr = serve(config.clone(), Arc::default()).await;
  let token = [("authorization", "Bearer ok")];
  let mut client = Client::connect(addr, &token).await;
  let session = client.session.clone();
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Secret.count", "payload": 50}))
    .await;
  let cursor = read_items(&mut client, 1).await;
  drop(client);
  // Same (anonymous) principal, but the token is gone: the guard refuses the resumed call.
  let mut client = Client::connect(addr, &[]).await;
  assert_eq!(client.resume(&session, &[(1, cursor)]).await, session);
  let exit = client.next().await.unwrap();
  assert_eq!(exit["payload"]["error"]["_tag"], "Rejected", "{exit}");
  assert_eq!(exit["payload"]["error"]["status"], 403);
  // The refused call is released.
  drop(client);
  until(|| config.resume_stats().calls == 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_buffer_released_after_retention() {
  let clock = TestClock::new();
  let config = config().resume(ResumeConfig::new().clock(clock.clone()));
  let released = Arc::new(AtomicBool::new(false));
  let addr = serve(config.clone(), released.clone()).await;
  let mut client = Client::connect(addr, &[]).await;
  let session = client.session.clone();
  client
    .send(json!({"id": 1, "kind": "call", "tag": "Count.forever", "payload": 0}))
    .await;
  read_items(&mut client, 32).await;
  // The window is spent: nothing more comes, and what was sent is held.
  assert!(
    client
      .next_within(Duration::from_millis(100))
      .await
      .is_none()
  );
  drop(client);
  until(|| config.resume_stats().detached == 1).await;
  let held = config.resume_stats();
  assert_eq!((held.sessions, held.calls, held.messages), (1, 1, 32));
  assert!(held.bytes > 0);
  assert!(
    !released.load(Ordering::SeqCst),
    "detached calls keep running"
  );

  clock.advance(Duration::from_secs(29));
  assert_eq!(config.release_expired_sessions(), 0);
  clock.advance(Duration::from_secs(1));
  assert_eq!(config.release_expired_sessions(), 1);
  let after = config.resume_stats();
  assert_eq!(
    (
      after.sessions,
      after.detached,
      after.calls,
      after.messages,
      after.bytes
    ),
    (0, 0, 0, 0, 0)
  );
  // The call was interrupted: its stream is dropped.
  until(|| released.load(Ordering::SeqCst)).await;
  // And the session is gone for good.
  let mut client = Client::connect(addr, &[]).await;
  let own = client.session.clone();
  assert_eq!(
    client.resume(&session, &[(1, Cursor::sequence(32))]).await,
    own
  );
  assert_eq!(
    client.next().await.unwrap()["payload"]["error"]["_tag"],
    "ResumeExpired"
  );
}
