//! The six procedure kinds over HTTP (design note §2): GET queries with ETags, idempotent
//! mutations, NDJSON server and client streams, bidi refused with 426, and the C5 / R9 rules for
//! every POST. The in-process transport covers two-way streams and subscriptions (their wire
//! mappings arrive with the WebSocket and SSE transports).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::{DynEnv, Never, Stream, fail, schema, succeed};
use id_effect_rpc::error_union::{RpcErrorStatus, RpcErrorUnion};
use id_effect_rpc::{Procedure, RpcGroup, RpcPrincipal, RpcServer, WireSchema};
use serde_json::{Value, json};
use tower::ServiceExt;

/// `NotFound { id }`, status 404.
struct Missing(i64);

impl RpcErrorStatus for Missing {
  fn status(&self) -> StatusCode {
    StatusCode::NOT_FOUND
  }
}

impl RpcErrorUnion for Missing {
  fn to_wire(&self) -> Value {
    json!({"_tag": "NotFound", "id": self.0})
  }
}

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

struct Fixture {
  app: axum::Router,
  adds: Arc<AtomicUsize>,
}

fn fixture() -> Fixture {
  let adds = Arc::new(AtomicUsize::new(0));
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Todo.get").public(),
    int(),
    text(),
    |id: i64, _: &mut DynEnv| {
      if id < 0 {
        fail(Missing(id))
      } else {
        succeed(format!("todo {id}"))
      }
    },
  );
  group.query(
    Procedure::query("Todo.get").version(2).public(),
    int(),
    text(),
    |id: i64, _: &mut DynEnv| succeed::<_, Never, _>(format!("todo v2 {id}")),
  );
  let counter = adds.clone();
  group.mutation(
    Procedure::mutation("Todo.add").public(),
    text(),
    int(),
    move |_title: String, _: &mut DynEnv| {
      let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
      succeed::<_, Never, _>(n as i64)
    },
  );
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.client_stream(
    Procedure::client_stream("Sum.all")
      .max_items(3)
      .max_bytes(64)
      .public(),
    int(),
    int(),
    |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
  );
  group.bidi(
    Procedure::bidi("Chat.room").public(),
    text(),
    text(),
    |_: &mut DynEnv| succeed::<_, Never, _>(id_effect_rpc::Duplex::new(|s| s)),
  );
  group.subscription(
    Procedure::subscription("Feed.watch").public(),
    text(),
    text(),
    |_: String, _, _: &mut DynEnv| {
      succeed::<_, Never, _>(id_effect_rpc::Subscription::from_stream(Stream::empty()))
    },
  );
  Fixture {
    app: RpcServer::new(group)
      .mount_default()
      .with_state(DynEnv::new()),
    adds,
  }
}

struct Reply {
  status: StatusCode,
  headers: axum::http::HeaderMap,
  bytes: Vec<u8>,
}

impl Reply {
  fn json(&self) -> Value {
    serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
  }
}

async fn send(app: &axum::Router, req: Request<Body>) -> Reply {
  let res = app.clone().oneshot(req).await.unwrap();
  let status = res.status();
  let headers = res.headers().clone();
  let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
  Reply {
    status,
    headers,
    bytes,
  }
}

fn post(uri: &str) -> axum::http::request::Builder {
  Request::post(uri)
    .header("content-type", "application/json")
    .header("x-rpc", "1")
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
  post(uri).body(Body::from(body.to_owned())).unwrap()
}

fn get(uri: &str) -> Request<Body> {
  Request::get(uri).body(Body::empty()).unwrap()
}

/// A capability an authentication layer puts on the request.
struct Caller(&'static str);

/// Procedures over plain HTTP see the request's capabilities over the route state, as the
/// WebSocket, SSE and WebTransport transports give their calls (the `CurrentPrincipal` an
/// `Authenticate` layer inserts reaches the handler).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_calls_see_the_request_capabilities() {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Me.who").public(),
    int(),
    text(),
    |_: i64, env: &mut DynEnv| match env.try_get::<Caller>() {
      Ok(caller) => succeed::<_, Never, DynEnv>(caller.0.to_owned()),
      Err(_) => succeed("anonymous".to_owned()),
    },
  );
  let app = RpcServer::new(group)
    .router()
    .with_state(DynEnv::new())
    .into_axum()
    .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 9))));
  let anonymous = send(&app, post_json("/rpc/Me.who", "0")).await;
  assert_eq!(anonymous.json()["exit"]["value"], "anonymous");

  let authenticated = app.layer(axum::middleware::from_fn(
    |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
      let mut caps = id_effect_axum::RequestCaps::new();
      caps.insert(Caller("ada"));
      req.extensions_mut().insert(caps);
      next.run(req).await
    },
  ));
  for req in [
    post_json("/rpc/Me.who", "0"),
    post_json("/rpc", r#"{"tag":"Me.who","payload":0}"#),
    get("/rpc/Me.who?input=0"),
  ] {
    let reply = send(&authenticated, req).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.json());
    assert_eq!(reply.json()["exit"]["value"], "ada");
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_get_returns_etag_and_304() {
  let Fixture { app, .. } = fixture();
  let first = send(&app, get("/rpc/Todo.get?input=7")).await;
  assert_eq!(first.status, StatusCode::OK);
  assert_eq!(
    first.json(),
    json!({"tag": "Todo.get", "exit": {"_tag": "Success", "value": "todo v2 7"}})
  );
  let etag = first.headers["etag"].to_str().unwrap().to_owned();
  assert!(etag.starts_with('"') && etag.ends_with('"'));
  assert_eq!(first.headers["cache-control"], "private, no-cache");

  let again = Request::get("/rpc/Todo.get?input=7")
    .header("if-none-match", &etag)
    .body(Body::empty())
    .unwrap();
  let not_modified = send(&app, again).await;
  assert_eq!(not_modified.status, StatusCode::NOT_MODIFIED);
  assert!(not_modified.bytes.is_empty());
  assert_eq!(not_modified.headers["etag"], etag.as_str());

  // Another input, another version: another tag.
  let v1 = Request::get("/rpc/Todo.get?input=7")
    .header("x-rpc-version", "1")
    .header("if-none-match", &etag)
    .body(Body::empty())
    .unwrap();
  let v1 = send(&app, v1).await;
  assert_eq!(v1.status, StatusCode::OK);
  assert_eq!(v1.json()["exit"]["value"], "todo 7");
  assert_ne!(v1.headers["etag"], etag.as_str());

  // A typed failure carries no ETag.
  let missing = Request::get("/rpc/Todo.get?input=-1")
    .header("x-rpc-version", "1")
    .body(Body::empty())
    .unwrap();
  let missing = send(&app, missing).await;
  assert_eq!(missing.status, StatusCode::NOT_FOUND);
  assert_eq!(
    missing.json()["exit"]["error"],
    json!({"_tag": "NotFound", "id": -1})
  );
  assert!(!missing.headers.contains_key("etag"));

  // Queries are also served over POST, with and without the envelope.
  let posted = send(&app, post_json("/rpc/Todo.get", "3")).await;
  assert_eq!(posted.json()["exit"]["value"], "todo v2 3");
  let envelope = send(
    &app,
    post_json("/rpc", r#"{"id":9,"tag":"Todo.get","v":1,"payload":3}"#),
  )
  .await;
  assert_eq!(envelope.json()["id"], 9);
  assert_eq!(envelope.json()["exit"]["value"], "todo 3");

  // Edge cases of the GET route.
  let bad = send(&app, get("/rpc/Todo.get?input=%7B")).await;
  assert_eq!(bad.status, StatusCode::BAD_REQUEST);
  let invalid = send(&app, get("/rpc/Todo.get?input=%22x%22")).await;
  assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
  let no_input = send(&app, get("/rpc/Todo.get")).await;
  assert_eq!(no_input.json()["exit"]["error"]["_tag"], "InvalidInput");
  let version = Request::get("/rpc/Todo.get?input=1")
    .header("x-rpc-version", "zero")
    .body(Body::empty())
    .unwrap();
  assert_eq!(send(&app, version).await.status, StatusCode::BAD_REQUEST);
  let unknown_version = Request::get("/rpc/Todo.get?input=1")
    .header("x-rpc-version", "7")
    .body(Body::empty())
    .unwrap();
  let unknown_version = send(&app, unknown_version).await;
  assert_eq!(unknown_version.status, StatusCode::NOT_FOUND);
  assert_eq!(unknown_version.json()["version"], 7);
  let unknown = send(&app, get("/rpc/Todo.nope")).await;
  assert_eq!(unknown.status, StatusCode::NOT_FOUND);
  assert_eq!(unknown.json()["tag"], "Todo.nope");
  let mutation = send(&app, get("/rpc/Todo.add?input=%22x%22")).await;
  assert_eq!(mutation.status, StatusCode::METHOD_NOT_ALLOWED);
  assert_eq!(mutation.headers["allow"], "POST");
  let stream = send(&app, get("/rpc/Count.up?input=1")).await;
  assert_eq!(stream.status, StatusCode::METHOD_NOT_ALLOWED);
}

fn idempotent(uri: &str, key: &str, body: &str) -> Request<Body> {
  post(uri)
    .header("idempotency-key", key)
    .body(Body::from(body.to_owned()))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutation_idempotency_key_replays_result() {
  let Fixture { app, adds } = fixture();
  let with_principal = |mut req: Request<Body>| {
    req.extensions_mut().insert(RpcPrincipal("ada".to_owned()));
    req
  };
  let first = send(
    &app,
    with_principal(idempotent("/rpc/Todo.add", "k1", r#""milk""#)),
  )
  .await;
  assert_eq!(first.status, StatusCode::OK);
  assert!(!first.headers.contains_key("idempotency-replayed"));
  let replay = send(
    &app,
    with_principal(idempotent("/rpc/Todo.add", "k1", r#""milk""#)),
  )
  .await;
  assert_eq!(replay.status, StatusCode::OK);
  assert_eq!(replay.headers["idempotency-replayed"], "true");
  assert_eq!(replay.bytes, first.bytes);
  assert_eq!(adds.load(Ordering::SeqCst), 1, "the handler ran once");

  // The envelope route shares the store (same tag, same key, same payload) and echoes its id.
  let envelope = send(
    &app,
    with_principal(idempotent(
      "/rpc",
      "k1",
      r#"{"id":5,"tag":"Todo.add","payload":"milk"}"#,
    )),
  )
  .await;
  assert_eq!(envelope.json()["id"], 5);
  assert_eq!(envelope.json()["exit"], first.json()["exit"]);
  assert_eq!(adds.load(Ordering::SeqCst), 1);

  // Another payload under the same key is refused; another principal has its own keys.
  let reused = send(
    &app,
    with_principal(idempotent("/rpc/Todo.add", "k1", r#""eggs""#)),
  )
  .await;
  assert_eq!(reused.status, StatusCode::CONFLICT);
  assert_eq!(reused.json()["code"], "IdempotencyKeyReused");
  let mut other = idempotent("/rpc/Todo.add", "k1", r#""milk""#);
  other
    .extensions_mut()
    .insert(RpcPrincipal("grace".to_owned()));
  assert_eq!(send(&app, other).await.status, StatusCode::OK);
  assert_eq!(adds.load(Ordering::SeqCst), 2);

  // Without a key every call runs; a malformed key is refused.
  send(&app, post_json("/rpc/Todo.add", r#""milk""#)).await;
  assert_eq!(adds.load(Ordering::SeqCst), 3);
  let malformed = send(
    &app,
    with_principal(idempotent("/rpc/Todo.add", "a b", r#""x""#)),
  )
  .await;
  assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
  assert_eq!(malformed.json()["code"], "IdempotencyKeyMalformed");
  // A key on a query is ignored.
  let query = send(&app, idempotent("/rpc/Todo.get", "k", "1")).await;
  assert_eq!(query.status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotency_replay_is_scoped_for_anonymous_callers() {
  let Fixture { app, adds } = fixture();
  let from = |ip: [u8; 4], key: &str| {
    let mut req = idempotent("/rpc/Todo.add", key, r#""milk""#);
    req
      .extensions_mut()
      .insert(MockConnectInfo(SocketAddr::from((ip, 4000))));
    req
  };
  let key = "anon-key-0123456789";
  let first = send(&app, from([192, 0, 2, 1], key)).await;
  assert_eq!(first.status, StatusCode::OK);
  let replay = send(&app, from([192, 0, 2, 1], key)).await;
  assert_eq!(replay.headers["idempotency-replayed"], "true");
  assert_eq!(adds.load(Ordering::SeqCst), 1);
  // The same key and payload from another address does not read the first caller's result.
  let other = send(&app, from([198, 51, 100, 7], key)).await;
  assert_eq!(other.status, StatusCode::OK);
  assert!(!other.headers.contains_key("idempotency-replayed"));
  assert_eq!(adds.load(Ordering::SeqCst), 2);
  // Anonymous keys must be at least 16 characters, and need a known client address.
  let short = send(&app, from([192, 0, 2, 1], "short")).await;
  assert_eq!(short.status, StatusCode::BAD_REQUEST);
  assert_eq!(short.json()["code"], "IdempotencyKeyTooShort");
  let unscoped = send(&app, idempotent("/rpc/Todo.add", key, r#""milk""#)).await;
  assert_eq!(unscoped.status, StatusCode::BAD_REQUEST);
  assert_eq!(unscoped.json()["code"], "IdempotencyKeyUnscoped");
  assert_eq!(adds.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_stream_answers_ndjson_frames() {
  let Fixture { app, .. } = fixture();
  let reply = send(&app, post_json("/rpc/Count.up", "3")).await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(reply.headers["content-type"], "application/x-ndjson");
  let frames: Vec<Value> = std::str::from_utf8(&reply.bytes)
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect();
  assert_eq!(
    frames,
    [
      json!({"id": 1, "kind": "item", "flags": 0, "payload": 1}),
      json!({"id": 1, "kind": "item", "flags": 0, "payload": 2}),
      json!({"id": 1, "kind": "item", "flags": 0, "payload": 3}),
      json!({"id": 1, "kind": "exit", "flags": 0, "payload": {"_tag": "Success", "value": null}}),
    ]
  );
  let invalid = send(&app, post_json("/rpc/Count.up", r#""x""#)).await;
  let last = std::str::from_utf8(&invalid.bytes)
    .unwrap()
    .lines()
    .last()
    .unwrap()
    .to_owned();
  assert_eq!(
    serde_json::from_str::<Value>(&last).unwrap()["payload"]["error"]["_tag"],
    "InvalidInput"
  );
  let malformed = send(&app, post_json("/rpc/Count.up", "{")).await;
  assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
  let envelope = send(&app, post_json("/rpc", r#"{"tag":"Count.up","payload":1}"#)).await;
  assert_eq!(envelope.status, StatusCode::BAD_REQUEST);
  assert_eq!(envelope.json()["kind"], "server_stream");
}

fn ndjson(uri: &str, body: &'static str) -> Request<Body> {
  Request::post(uri)
    .header("content-type", "application/x-ndjson")
    .header("x-rpc", "1")
    .body(Body::from(body))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_stream_over_cap_rejected() {
  let Fixture { app, .. } = fixture();
  let ok = send(&app, ndjson("/rpc/Sum.all", "1\n2\n3\n")).await;
  assert_eq!(ok.status, StatusCode::OK);
  assert_eq!(ok.json()["exit"]["value"], 6);

  let items = send(&app, ndjson("/rpc/Sum.all", "1\n2\n3\n4\n5\n")).await;
  assert_eq!(items.status, StatusCode::TOO_MANY_REQUESTS);
  assert_eq!(
    items.json()["exit"]["error"],
    json!({"_tag": "ResourceExhausted", "limit": "max_items"})
  );
  let bytes = send(
    &app,
    ndjson(
      "/rpc/Sum.all",
      "1000000000000000000000000000000000000000000000000000000000000000000000\n",
    ),
  )
  .await;
  assert_eq!(bytes.json()["exit"]["error"]["limit"], "max_bytes");

  let not_json = send(&app, ndjson("/rpc/Sum.all", "1\n{\n")).await;
  assert_eq!(not_json.status, StatusCode::UNPROCESSABLE_ENTITY);
  let wrong_type = send(&app, ndjson("/rpc/Sum.all", "1\n\"two\"\n")).await;
  assert_eq!(wrong_type.status, StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(wrong_type.json()["exit"]["error"]["_tag"], "InvalidInput");
  // A client stream is sent as NDJSON.
  let json_body = send(&app, post_json("/rpc/Sum.all", "[1,2]")).await;
  assert_eq!(json_body.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidi_over_plain_http_is_426_and_subscriptions_406() {
  let Fixture { app, .. } = fixture();
  for req in [
    post_json("/rpc/Chat.room", "null"),
    get("/rpc/Chat.room"),
    post_json("/rpc", r#"{"tag":"Chat.room","payload":null}"#),
  ] {
    let reply = send(&app, req).await;
    assert_eq!(reply.status, StatusCode::UPGRADE_REQUIRED);
    assert_eq!(reply.headers["upgrade"], "websocket");
    assert_eq!(reply.json()["kind"], "bidi");
  }
  for req in [
    post_json("/rpc/Feed.watch", r#""a""#),
    get("/rpc/Feed.watch"),
  ] {
    assert_eq!(send(&app, req).await.status, StatusCode::NOT_ACCEPTABLE);
  }
  let envelope = send(
    &app,
    post_json("/rpc", r#"{"tag":"Feed.watch","payload":"a"}"#),
  )
  .await;
  assert_eq!(envelope.status, StatusCode::BAD_REQUEST);
  let unknown = send(&app, post_json("/rpc/Nope.nope", "1")).await;
  assert_eq!(unknown.status, StatusCode::NOT_FOUND);
  let version = post("/rpc/Todo.get")
    .header("x-rpc-version", "0")
    .body(Body::from("1"))
    .unwrap();
  assert_eq!(send(&app, version).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r9_rpc_mutation_without_custom_header_refused() {
  let Fixture { app, adds } = fixture();
  for uri in ["/rpc/Todo.add", "/rpc"] {
    let body = if uri == "/rpc" {
      r#"{"tag":"Todo.add","payload":"milk"}"#
    } else {
      r#""milk""#
    };
    let req = Request::post(uri)
      .header("content-type", "application/json")
      .body(Body::from(body))
      .unwrap();
    let reply = send(&app, req).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{uri}");
    assert_eq!(reply.json()["code"], "RpcHeaderRequired");
    assert_eq!(reply.headers["content-type"], "application/problem+json");
    let wrong = Request::post(uri)
      .header("content-type", "application/json")
      .header("x-rpc", "yes")
      .body(Body::from(body))
      .unwrap();
    assert_eq!(send(&app, wrong).await.status, StatusCode::FORBIDDEN);
  }
  assert_eq!(adds.load(Ordering::SeqCst), 0, "the handler never ran");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r9_rpc_form_content_type_refused() {
  let Fixture { app, adds } = fixture();
  for content_type in [
    "application/x-www-form-urlencoded",
    "multipart/form-data; boundary=x",
    "text/plain",
  ] {
    for uri in ["/rpc/Todo.add", "/rpc"] {
      let req = Request::post(uri)
        .header("content-type", content_type)
        .header("x-rpc", "1")
        .body(Body::from("title=milk"))
        .unwrap();
      let reply = send(&app, req).await;
      assert_eq!(
        reply.status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "{content_type} {uri}"
      );
      assert_eq!(reply.headers["content-type"], "application/problem+json");
    }
  }
  let none = Request::post("/rpc/Todo.add")
    .header("x-rpc", "1")
    .body(Body::from(r#""milk""#))
    .unwrap();
  assert_eq!(
    send(&app, none).await.status,
    StatusCode::UNSUPPORTED_MEDIA_TYPE
  );
  assert_eq!(adds.load(Ordering::SeqCst), 0, "the handler never ran");
}

/// Admits requests carrying `x-staff: yes`.
struct Staff;

impl id_effect_rpc::registry::RouteGuard for Staff {
  fn policy(&self) -> &str {
    "staff"
  }

  fn check<'a>(
    &'a self,
    parts: &'a axum::http::request::Parts,
  ) -> id_effect_axum::route::GuardFuture<'a> {
    let staff = parts.headers.get("x-staff").is_some_and(|v| v == "yes");
    Box::pin(async move {
      if staff {
        Ok(())
      } else {
        Err(axum::response::IntoResponse::into_response(
          StatusCode::FORBIDDEN,
        ))
      }
    })
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tag_routes_run_guards_caps_and_reservations() {
  use id_effect_rpc::idempotency::{
    Begin, IdempotencyKey, IdempotencyScope, IdempotencyStore, MemoryIdempotencyStore,
    payload_digest,
  };
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Admin.stats").guard(Staff),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n),
  );
  group.mutation(
    Procedure::mutation("Admin.reset").guard(Staff),
    text(),
    text(),
    |s: String, _: &mut DynEnv| succeed::<_, Never, _>(s),
  );
  let store = MemoryIdempotencyStore::new();
  let app = RpcServer::new(group)
    .with_max_body_bytes(16)
    .with_idempotency_store(store.clone())
    .mount_default()
    .with_state(DynEnv::new());

  assert_eq!(
    send(&app, get("/rpc/Admin.stats?input=1")).await.status,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    send(&app, post_json("/rpc/Admin.stats", "1")).await.status,
    StatusCode::FORBIDDEN
  );
  let staff_get = Request::get("/rpc/Admin.stats?input=4")
    .header("x-staff", "yes")
    .body(Body::empty())
    .unwrap();
  assert_eq!(send(&app, staff_get).await.json()["exit"]["value"], 4);
  let big = post("/rpc/Admin.stats")
    .header("x-staff", "yes")
    .body(Body::from("1".repeat(17)))
    .unwrap();
  assert_eq!(send(&app, big).await.status, StatusCode::PAYLOAD_TOO_LARGE);

  // A key whose first call is still running is refused, not run twice.
  let key = IdempotencyKey::new(
    IdempotencyScope::Principal("ada".into()),
    "Admin.reset@1",
    "k",
  );
  let digest = payload_digest("Admin.reset@1", br#""now""#);
  assert_eq!(store.begin(&key, digest).await, Begin::Fresh);
  let mut retry = post("/rpc/Admin.reset")
    .header("x-staff", "yes")
    .header("idempotency-key", "k")
    .body(Body::from(r#""now""#))
    .unwrap();
  retry
    .extensions_mut()
    .insert(RpcPrincipal("ada".to_owned()));
  let reply = send(&app, retry).await;
  assert_eq!(reply.status, StatusCode::CONFLICT);
  assert_eq!(reply.json()["code"], "IdempotencyKeyInProgress");
}

#[cfg(feature = "test-util")]
mod in_process {
  use super::*;
  use id_effect_rpc::kinds::{InProcess, SubscriptionItem};
  use id_effect_rpc::server::dispatch::Rejection;
  use id_effect_rpc::wire::{Cursor, FrameKind};

  fn group() -> RpcGroup {
    let mut group = RpcGroup::new();
    group.bidi(
      Procedure::bidi("Chat.shout").public(),
      text(),
      text(),
      |_: &mut DynEnv| {
        succeed::<_, Never, _>(id_effect_rpc::Duplex::new(
          |items: Stream<String, Never, DynEnv>| items.map(|s| s.to_uppercase()),
        ))
      },
    );
    group.subscription(
      Procedure::subscription("Feed.watch").public(),
      text(),
      text(),
      |prefix: String, after: Option<Cursor>, _: &mut DynEnv| {
        let start = after.map_or(1, |c| c.position + 1);
        let items = (start..=3)
          .map(|n| SubscriptionItem::new(format!("{prefix}{n}")).with_cursor(Cursor::events(n)))
          .collect();
        succeed::<_, Never, _>(id_effect_rpc::Subscription::new(Stream::from_vec(items)))
      },
    );
    group.client_stream(
      Procedure::client_stream("Sum.all").public(),
      int(),
      int(),
      |items: Stream<i64, Never, DynEnv>, _: &mut DynEnv| items.run_fold(0, |a, b| a + b),
    );
    group.server_stream(
      Procedure::server_stream("Count.up").public(),
      int(),
      int(),
      |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
    );
    group.query(
      Procedure::query("Todo.get").public(),
      int(),
      text(),
      |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n.to_string()),
    );
    group
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn every_kind_runs_in_process() {
    let rpc = InProcess::new(group(), DynEnv::new());
    assert!(format!("{rpc:?}").contains("procedures"));

    let (tx, frames) = rpc.bidi("Chat.shout").await.unwrap();
    assert!(tx.send(json!("hi")).await);
    assert!(tx.send(json!("there")).await);
    drop(tx);
    let frames = frames.collect().await;
    let payloads: Vec<_> = frames.iter().map(|f| f.payload.clone().unwrap()).collect();
    assert_eq!(payloads[..2], [json!("HI"), json!("THERE")]);
    assert_eq!(frames[2].kind, FrameKind::Exit);

    let frames = rpc
      .subscribe("Feed.watch", json!("e"), Some(Cursor::events(1)))
      .await
      .unwrap()
      .collect()
      .await;
    assert_eq!(frames[0].payload, Some(json!("e2")));
    assert_eq!(frames[0].cursor, Some(Cursor::events(2)));
    assert_eq!(frames[1].cursor, Some(Cursor::events(3)));
    assert_eq!(frames[2].kind, FrameKind::Exit);

    let sum = rpc
      .client_stream("Sum.all", vec![json!(1), json!(2)])
      .await
      .unwrap();
    assert_eq!(
      sum.exit,
      id_effect_rpc::wire::WireExit::Success { value: json!(3) }
    );
    let bad = rpc
      .client_stream("Sum.all", vec![json!(1), json!("x"), json!(2)])
      .await
      .unwrap();
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

    let frames = rpc
      .server_stream("Count.up", json!(2))
      .await
      .unwrap()
      .collect()
      .await;
    assert_eq!(frames.len(), 3);
    let one = rpc.unary("Todo.get", json!(5)).await.unwrap();
    assert!(one.exit.is_success());
    assert!(matches!(
      rpc.unary("Count.up", json!(1)).await,
      Err(Rejection::WrongKind { .. })
    ));
    assert!(matches!(
      rpc.unary("Nope.nope", json!(1)).await,
      Err(Rejection::UnknownProcedure { .. })
    ));
  }
}
