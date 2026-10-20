//! Batches (design note §2.2, r12-e12-1-batching): request order and per-call failures, one
//! `RequestCache` per batch, ordered and bounded execution, the R4 caps, the N-token rate limit
//! charge, idempotent mutations, and the Rust client's same-tick batches.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use id_effect::{
  Concurrency, DynEnv, Effect, Never, Request as EffectRequest, RequestResolver, Stream,
  TestExecutor, all, fail, from_async, hlist, schema, succeed, sync,
};
use id_effect_axum::RateLimitLayer;
use id_effect_axum::middleware::{MemoryRateLimitStore, RateLimitKey};
use id_effect_axum::route::GuardFuture;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpClientService, HttpRequest, HttpResponse, StreamingHttpResponse,
};
use id_effect_rpc::batch::{BATCH_INDEX_HEADER, BatchConfig, ORDERED_HEADER};
use id_effect_rpc::error_union::{RpcErrorStatus, RpcErrorUnion};
use id_effect_rpc::registry::RouteGuard;
use id_effect_rpc::wire::{MAX_BATCH_CALLS, decode_batch};
use id_effect_rpc::{Procedure, RpcClient, RpcGroup, RpcPrincipal, RpcServer, WireSchema};
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

/// Sleep `ms` inside a handler's effect.
fn after<A: Send + 'static>(ms: u64, value: A) -> Effect<A, Never, DynEnv> {
  from_async(move || async move {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    Ok(value)
  })
}

/// Refuses every call (a guard's 403).
struct Deny;

impl RouteGuard for Deny {
  fn policy(&self) -> &str {
    "deny"
  }

  fn check<'a>(&'a self, _parts: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async { Err(StatusCode::FORBIDDEN.into_response()) })
  }
}

/// Calls in flight now, the most at once, and the order they started in.
#[derive(Clone, Default)]
struct Probe {
  now: Arc<AtomicUsize>,
  most: Arc<AtomicUsize>,
  started: Arc<Mutex<Vec<i64>>>,
}

impl Probe {
  fn run(&self, n: i64) -> Effect<i64, Never, DynEnv> {
    let probe = self.clone();
    from_async(move || async move {
      probe.started.lock().unwrap().push(n);
      let now = probe.now.fetch_add(1, Ordering::SeqCst) + 1;
      probe.most.fetch_max(now, Ordering::SeqCst);
      tokio::time::sleep(Duration::from_millis(30)).await;
      probe.now.fetch_sub(1, Ordering::SeqCst);
      Ok(n)
    })
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct GetUser(i64);

impl EffectRequest for GetUser {
  type Output = String;
  type Error = Never;
}

struct Fixture {
  server: RpcServer,
  adds: Arc<AtomicUsize>,
  probe: Probe,
  /// The keys of every resolver call.
  loads: Arc<Mutex<Vec<Vec<i64>>>>,
}

fn fixture() -> Fixture {
  let adds = Arc::new(AtomicUsize::new(0));
  let probe = Probe::default();
  let loads: Arc<Mutex<Vec<Vec<i64>>>> = Arc::default();
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
    Procedure::query("Slow.echo").public(),
    int(),
    int(),
    |ms: i64, _: &mut DynEnv| after(ms.unsigned_abs(), ms),
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
  group.query(
    Procedure::query("Secret.get").guard(Deny),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n),
  );
  let p = probe.clone();
  group.query(
    Procedure::query("Probe.run").public(),
    int(),
    int(),
    move |n: i64, _: &mut DynEnv| p.run(n),
  );
  let seen = loads.clone();
  let users: RequestResolver<GetUser, DynEnv> = RequestResolver::make_batched(move |reqs| {
    seen
      .lock()
      .unwrap()
      .push(reqs.iter().map(|GetUser(id)| *id).collect());
    sync(move || {
      reqs
        .into_iter()
        .map(|GetUser(id)| Ok(format!("user-{id}")))
        .collect()
    })
  });
  group.query(
    Procedure::query("User.name").public(),
    int(),
    text(),
    move |id: i64, _: &mut DynEnv| Effect::request(GetUser(id), &users),
  );
  Fixture {
    server: RpcServer::new(group),
    adds,
    probe,
    loads,
  }
}

fn app(server: RpcServer) -> axum::Router {
  server.mount_default().with_state(DynEnv::new())
}

struct Reply {
  status: StatusCode,
  headers: axum::http::HeaderMap,
  json: Value,
}

async fn send(app: &axum::Router, req: Request<Body>) -> Reply {
  let res = app.clone().oneshot(req).await.unwrap();
  let status = res.status();
  let headers = res.headers().clone();
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  Reply {
    status,
    headers,
    json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
  }
}

fn rpc() -> axum::http::request::Builder {
  Request::post("/rpc")
    .header("content-type", "application/json")
    .header("x-rpc", "1")
}

fn with_body(builder: axum::http::request::Builder, body: &Value) -> Request<Body> {
  builder
    .body(Body::from(serde_json::to_vec(body).unwrap()))
    .unwrap()
}

fn post(body: Value) -> Request<Body> {
  with_body(rpc(), &body)
}

fn calls(tag: &str, inputs: &[i64]) -> Value {
  Value::Array(
    inputs
      .iter()
      .zip(1..)
      .map(|(input, id)| json!({"id": id, "tag": tag, "payload": input}))
      .collect(),
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_results_in_request_order_each_fails_alone() {
  let fx = fixture();
  let app = app(fx.server);
  let body = json!([
    {"id": 10, "tag": "Slow.echo", "payload": 60},
    {"id": 3, "tag": "Todo.get", "payload": 1},
    {"id": 7, "tag": "Todo.get", "payload": -1},
    {"id": 1, "tag": "Nope.never", "payload": 1},
    {"id": 2, "tag": "Todo.get", "v": 9, "payload": 1},
    {"id": 4, "tag": "Count.up", "payload": 3},
    {"id": 5, "tag": "Todo.get", "payload": "not a number"},
    {"id": 6, "tag": "Todo.add", "payload": "milk"},
  ]);
  let reply = send(&app, post(body)).await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(reply.headers["content-type"], "application/json");
  let results = reply.json.as_array().unwrap();
  let ids: Vec<i64> = results.iter().map(|r| r["id"].as_i64().unwrap()).collect();
  assert_eq!(
    ids,
    [10, 3, 7, 1, 2, 4, 5, 6],
    "request order, not completion order"
  );
  assert_eq!(results[0]["exit"], json!({"_tag": "Success", "value": 60}));
  assert_eq!(
    results[1]["exit"],
    json!({"_tag": "Success", "value": "todo 1"})
  );
  assert_eq!(
    results[2]["exit"],
    json!({"_tag": "Failure", "error": {"_tag": "NotFound", "id": -1}})
  );
  for (i, needle) in [
    (3, "unknown rpc procedure `Nope.never`"),
    (4, "at version 9"),
    (5, "server_stream"),
  ] {
    let error = &results[i]["exit"]["error"];
    assert_eq!(error["_tag"], "InvalidInput", "{i}");
    assert_eq!(error["issues"][0]["path"], "/tag");
    assert!(
      error["issues"][0]["message"]
        .as_str()
        .unwrap()
        .contains(needle),
      "{error}"
    );
  }
  assert_eq!(results[6]["exit"]["error"]["_tag"], "InvalidInput");
  assert_eq!(results[6]["tag"], "Todo.get");
  assert_eq!(results[7]["exit"], json!({"_tag": "Success", "value": 1}));
  assert_eq!(fx.adds.load(Ordering::SeqCst), 1);

  // A guard refusal answers the whole batch with the guard's response; nothing runs.
  let refused = send(
    &app,
    post(json!([
      {"id": 1, "tag": "Todo.add", "payload": "eggs"},
      {"id": 2, "tag": "Secret.get", "payload": 1},
    ])),
  )
  .await;
  assert_eq!(refused.status, StatusCode::FORBIDDEN);
  assert_eq!(refused.headers[BATCH_INDEX_HEADER], "1");
  assert_eq!(fx.adds.load(Ordering::SeqCst), 1);

  // Malformed batches are Problems.
  for (body, code) in [
    (json!([]), "BatchEmpty"),
    (
      json!([{"tag": "Todo.get", "payload": 1}]),
      "BatchCallIdMissing",
    ),
    (
      json!([{"id": 1, "tag": "Todo.get", "payload": 1}, {"id": 1, "tag": "Todo.get", "payload": 2}]),
      "BatchCallIdDuplicate",
    ),
    (json!([{"id": 1}]), "BatchMalformed"),
  ] {
    let reply = send(&app, post(body)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json["code"], code);
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_dedups_duplicate_loads_through_request_cache() {
  let fx = fixture();
  let app = app(fx.server);
  let reply = send(&app, post(calls("User.name", &[1, 2, 1, 1, 2]))).await;
  assert_eq!(reply.status, StatusCode::OK);
  let names: Vec<&str> = reply
    .json
    .as_array()
    .unwrap()
    .iter()
    .map(|r| r["exit"]["value"].as_str().unwrap())
    .collect();
  assert_eq!(names, ["user-1", "user-2", "user-1", "user-1", "user-2"]);
  let mut keys: Vec<i64> = fx.loads.lock().unwrap().concat();
  keys.sort_unstable();
  assert_eq!(keys, [1, 2], "each distinct load reached the resolver once");

  // Ordered: the cache still answers the repeats of the same batch.
  let ordered = with_body(
    rpc().header(ORDERED_HEADER, "true"),
    &calls("User.name", &[3, 3, 3]),
  );
  assert_eq!(send(&app, ordered).await.status, StatusCode::OK);
  // A new batch has a new cache: user 1 is loaded again.
  assert_eq!(
    send(&app, post(calls("User.name", &[1]))).await.status,
    StatusCode::OK
  );
  let mut keys: Vec<i64> = fx.loads.lock().unwrap().concat();
  keys.sort_unstable();
  assert_eq!(keys, [1, 1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordered_batch_runs_sequentially() {
  let fx = fixture();
  let probe = fx.probe.clone();
  let app = app(
    fx.server
      .with_batch_config(BatchConfig::new().with_parallelism(2)),
  );

  let ordered = with_body(
    rpc().header(ORDERED_HEADER, "true"),
    &calls("Probe.run", &[4, 3, 2, 1]),
  );
  let reply = send(&app, ordered).await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(probe.most.load(Ordering::SeqCst), 1);
  assert_eq!(*probe.started.lock().unwrap(), [4, 3, 2, 1]);

  // Unordered: in parallel, bounded by the admission controller (2 here).
  probe.started.lock().unwrap().clear();
  let reply = send(&app, post(calls("Probe.run", &[1, 2, 3, 4, 5, 6]))).await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(probe.most.load(Ordering::SeqCst), 2);
  assert_eq!(probe.started.lock().unwrap().len(), 6);
  let values: Vec<i64> = reply
    .json
    .as_array()
    .unwrap()
    .iter()
    .map(|r| r["exit"]["value"].as_i64().unwrap())
    .collect();
  assert_eq!(values, [1, 2, 3, 4, 5, 6]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_deadline_bounds_admission_and_calls() {
  let fx = fixture();
  let app = app(
    fx.server
      .with_batch_config(BatchConfig::new().with_parallelism(1)),
  );
  let req = with_body(
    rpc().header("x-rpc-deadline-ms", "50"),
    &calls("Slow.echo", &[300, 300]),
  );
  let reply = send(&app, req).await;
  assert_eq!(reply.status, StatusCode::OK);
  for result in reply.json.as_array().unwrap() {
    assert_eq!(result["exit"]["error"]["_tag"], "DeadlineExceeded");
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_batch_over_limit_rejected_413() {
  let fx = fixture();
  let adds = fx.adds.clone();
  let app = app(
    fx.server
      .with_batch_config(BatchConfig::new().with_max_bytes(4096)),
  );

  let too_many: Vec<Value> = (1..=MAX_BATCH_CALLS as i64 + 1)
    .map(|id| json!({"id": id, "tag": "Todo.add", "payload": "x"}))
    .collect();
  let reply = send(&app, post(Value::Array(too_many))).await;
  assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
  assert_eq!(reply.json["code"], "BatchTooManyCalls");
  // Elements that are not even calls: the count is checked before any is decoded.
  let junk = Value::Array(vec![json!(0); MAX_BATCH_CALLS + 1]);
  assert_eq!(
    send(&app, post(junk)).await.json["code"],
    "BatchTooManyCalls"
  );
  let big = json!([{"id": 1, "tag": "Todo.add", "payload": "y".repeat(5000)}]);
  let reply = send(&app, post(big)).await;
  assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
  assert_eq!(reply.json["code"], "BatchTooLarge");
  assert_eq!(
    adds.load(Ordering::SeqCst),
    0,
    "no call of a refused batch ran"
  );

  // Exactly at the cap is served.
  let fifty: Vec<Value> = (1..=MAX_BATCH_CALLS as i64)
    .map(|id| json!({"id": id, "tag": "Todo.get", "payload": id}))
    .collect();
  let body = serde_json::to_vec(&fifty).unwrap();
  assert_eq!(
    decode_batch(&body, MAX_BATCH_CALLS, 1 << 20).unwrap().len(),
    50
  );
  let app = app_default();
  assert_eq!(
    send(&app, post(Value::Array(fifty))).await.status,
    StatusCode::OK
  );
}

fn app_default() -> axum::Router {
  app(fixture().server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r4_batch_charges_n_rate_limit_tokens() {
  let fx = fixture();
  // Five tokens, no refill, one bucket.
  let limited: axum::Router = fx
    .server
    .mount_default()
    .layer(RateLimitLayer::new(MemoryRateLimitStore::new(0.0, 5.0)).key(RateLimitKey::global()))
    .with_state(DynEnv::new());
  let three = || post(calls("Todo.get", &[1, 2, 3]));
  let single = || post(json!({"tag": "Todo.get", "payload": 1}));
  assert_eq!(send(&limited, three()).await.status, StatusCode::OK); // 5 → 2
  let refused = send(&limited, three()).await; // the layer takes 1, the batch needs 2 more
  assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
  assert!(refused.headers.contains_key("retry-after"));
  assert_eq!(send(&limited, single()).await.status, StatusCode::OK); // the last token
  assert_eq!(
    send(&limited, single()).await.status,
    StatusCode::TOO_MANY_REQUESTS
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_mutations_keep_idempotency_key_semantics() {
  let fx = fixture();
  let adds = fx.adds.clone();
  let app = fx
    .server
    .mount_default()
    .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 9], 1234))))
    .with_state(DynEnv::new());
  let keyed = |key: &str, body: &Value| with_body(rpc().header("idempotency-key", key), body);
  let body = json!([
    {"id": 1, "tag": "Todo.add", "payload": "a"},
    {"id": 2, "tag": "Todo.get", "payload": 1},
    {"id": 3, "tag": "Todo.add", "payload": "b"},
  ]);
  let key = "batch-key-0123456789";
  let first = send(&app, keyed(key, &body)).await;
  assert_eq!(first.status, StatusCode::OK);
  assert_eq!(adds.load(Ordering::SeqCst), 2);
  let again = send(&app, keyed(key, &body)).await;
  assert_eq!(
    again.json, first.json,
    "a retried batch replays its mutations"
  );
  assert_eq!(adds.load(Ordering::SeqCst), 2);

  // Another payload under a used key: 409, nothing runs, the fresh reservations are released.
  let changed = json!([
    {"id": 9, "tag": "Todo.add", "payload": "c"},
    {"id": 3, "tag": "Todo.add", "payload": "other"},
  ]);
  let conflict = send(&app, keyed(key, &changed)).await;
  assert_eq!(conflict.status, StatusCode::CONFLICT);
  assert_eq!(conflict.json["code"], "IdempotencyKeyReused");
  assert_eq!(conflict.headers[BATCH_INDEX_HEADER], "1");
  assert_eq!(adds.load(Ordering::SeqCst), 2);
  let only_new = json!([{"id": 9, "tag": "Todo.add", "payload": "c"}]);
  assert_eq!(
    send(&app, keyed(key, &only_new)).await.status,
    StatusCode::OK
  );
  assert_eq!(
    adds.load(Ordering::SeqCst),
    3,
    "call 9's reservation was released"
  );

  // Anonymous keys must be long; an authenticated principal's need not.
  let short = send(&app, keyed("short", &body)).await;
  assert_eq!(short.json["code"], "IdempotencyKeyTooShort");
  let mut authed = keyed("k", &body);
  authed.extensions_mut().insert(RpcPrincipal("ada".into()));
  assert_eq!(send(&app, authed).await.status, StatusCode::OK);

  // Without connect info an anonymous key has no scope.
  let bare = app_default();
  let unscoped = send(&bare, keyed(key, &body)).await;
  assert_eq!(unscoped.json["code"], "IdempotencyKeyUnscoped");
}

/// Answers batches and single calls with `Echo` results; records the bodies it got.
struct EchoServer {
  bodies: Mutex<Vec<Value>>,
}

impl HttpClient for EchoServer {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    let body: Value = serde_json::from_slice(req.body.as_deref().unwrap_or_default()).unwrap();
    self.bodies.lock().unwrap().push(body.clone());
    let echo = |call: &Value| json!({"id": call.get("id"), "tag": call["tag"], "exit": {"_tag": "Success", "value": call["payload"]["text"]}});
    let answer = match &body {
      Value::Array(calls) => Value::Array(calls.iter().map(echo).collect()),
      call => echo(call),
    };
    succeed(HttpResponse {
      status: 200,
      headers: vec![],
      body: serde_json::to_vec(&answer).unwrap(),
    })
  }

  fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    fail(HttpError::InvalidRequest("no streams".to_owned()))
  }
}

/// Run `eff` to completion on the FIFO test executor, where the children of one `all` always
/// share a batch window.
fn run_fifo<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, String> {
  let exec = TestExecutor::new();
  let handle = eff.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  let mut join = std::pin::pin!(handle.join_future());
  match join
    .as_mut()
    .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
  {
    std::task::Poll::Ready(exit) => exit.into_result().map_err(|_| "failed".to_owned()),
    std::task::Poll::Pending => Err("still running".to_owned()),
  }
}

#[test]
fn client_batches_same_tick_calls() {
  let server = Arc::new(EchoServer {
    bodies: Mutex::new(Vec::new()),
  });
  let env = || hlist![HttpClientService(server.clone() as Arc<dyn HttpClient>)];
  let client = RpcClient::new("http://rpc.test");
  let input = schema::struct_("text", schema::string::<()>(), "n", schema::i64::<()>());
  let output = schema::string::<()>();
  let say = |text: &str| {
    client.call(
      "Echo.say",
      (text.to_owned(), 1_i64),
      &input,
      &["text", "n"],
      &output,
    )
  };
  let three = all([say("a"), say("b"), say("c")], Concurrency::Unbounded);
  assert_eq!(run_fifo(three.provide_all(env())).unwrap(), ["a", "b", "c"]);
  {
    let bodies = server.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1, "one request for the tick: {bodies:?}");
    let ids: Vec<u64> = bodies[0]
      .as_array()
      .unwrap()
      .iter()
      .map(|c| c["id"].as_u64().unwrap())
      .collect();
    assert_eq!(ids, [1, 2, 3]);
  }

  // A lone call is a plain envelope; an opted-out call goes on its own.
  assert_eq!(run_fifo(say("solo").provide_all(env())).unwrap(), "solo");
  let unbatched = client.call_unbatched(
    "Echo.say",
    ("own".to_owned(), 2_i64),
    &input,
    &["text", "n"],
    &output,
  );
  let mixed = all([unbatched, say("x"), say("y")], Concurrency::Unbounded);
  assert_eq!(
    run_fifo(mixed.provide_all(env())).unwrap(),
    ["own", "x", "y"]
  );
  let bodies = server.bodies.lock().unwrap();
  assert_eq!(
    bodies[1],
    json!({"tag": "Echo.say", "payload": {"text": "solo", "n": 1}})
  );
  assert_eq!(
    bodies.len(),
    4,
    "the opted-out call and one batch of two: {bodies:?}"
  );
  assert!(bodies[2..].iter().any(|b| b["payload"]["text"] == "own"));
  assert!(
    bodies[2..]
      .iter()
      .any(|b| b.as_array().is_some_and(|a| a.len() == 2))
  );
}

/// An `HttpClient` that serves requests with an in-process app (no network).
#[cfg(feature = "postcard")]
struct InApp(axum::Router);

#[cfg(feature = "postcard")]
impl HttpClient for InApp {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    let app = self.0.clone();
    from_async(move || async move {
      let path = req.url.trim_start_matches("http://in.app").to_owned();
      let mut builder = Request::post(path);
      for (name, value) in &req.headers {
        builder = builder.header(name.as_str(), value.as_str());
      }
      let res = app
        .oneshot(
          builder
            .body(Body::from(req.body.unwrap_or_default()))
            .unwrap(),
        )
        .await
        .unwrap();
      let status = res.status().as_u16();
      let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
      Ok(HttpResponse {
        status,
        headers: vec![],
        body,
      })
    })
  }

  fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    fail(HttpError::InvalidRequest("no streams".to_owned()))
  }
}

#[cfg(feature = "postcard")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postcard_http_round_trip_query_and_mutation() {
  use id_effect_rpc::RpcClientError;
  use id_effect_rpc::codec::Codec as _;
  use id_effect_rpc::codec::postcard::{MEDIA_TYPE, Postcard, decode_result, encode_call};
  use id_effect_rpc::wire::{CallId, WireCall, WireExit};

  let adds = Arc::new(AtomicUsize::new(0));
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Pc.double").public(),
    int().with_serde(),
    int().with_serde(),
    |n: i64, _: &mut DynEnv| {
      if n < 0 {
        fail(Missing(n))
      } else {
        succeed(n * 2)
      }
    },
  );
  let counter = adds.clone();
  group.mutation(
    Procedure::mutation("Pc.add").public(),
    text().with_serde(),
    int().with_serde(),
    move |_title: String, _: &mut DynEnv| {
      succeed::<_, Never, _>(counter.fetch_add(1, Ordering::SeqCst) as i64 + 1)
    },
  );
  group.query(
    Procedure::query("Pc.json_only").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(n),
  );
  group.server_stream(
    Procedure::server_stream("Pc.count").public(),
    int().with_serde(),
    int().with_serde(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.query(
    Procedure::query("Pc.big").public(),
    int().with_serde(),
    text().with_serde(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>("x".repeat(n as usize)),
  );
  let app = app(RpcServer::new(group));
  let pc = |uri: &str, body: Vec<u8>| {
    Request::post(uri)
      .header("content-type", MEDIA_TYPE)
      .header("x-rpc", "1")
      .body(Body::from(body))
      .unwrap()
  };
  let raw = |req: Request<Body>| {
    let app = app.clone();
    async move {
      let res = app.oneshot(req).await.unwrap();
      let status = res.status();
      let content_type = res.headers()["content-type"].to_str().unwrap().to_owned();
      let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
      (status, content_type, body)
    }
  };
  let success = |body: &[u8]| -> i64 {
    match decode_result(body).unwrap().exit {
      WireExit::Success { value } => Postcard::decode(&value).unwrap(),
      WireExit::Failure { error } => panic!("failed: {error}"),
    }
  };

  // A query through the envelope, a mutation through its own route.
  let call =
    WireCall::new("Pc.double", Postcard::encode(&21_i64).unwrap()).with_id(CallId::new(5).unwrap());
  let (status, content_type, body) = raw(pc("/rpc", encode_call(&call).unwrap())).await;
  assert_eq!(
    (status, content_type.as_str()),
    (StatusCode::OK, MEDIA_TYPE)
  );
  assert_eq!(decode_result(&body).unwrap().id, CallId::new(5).ok());
  assert_eq!(success(&body), 42);
  let milk = Postcard::encode(&"milk".to_owned()).unwrap();
  let (status, _, body) = raw(pc("/rpc/Pc.add", milk)).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(success(&body), 1);

  // A typed failure keeps its status and its error union.
  let (status, _, body) = raw(pc("/rpc/Pc.double", Postcard::encode(&-3_i64).unwrap())).await;
  assert_eq!(status, StatusCode::NOT_FOUND);
  assert!(matches!(
    decode_result(&body).unwrap().exit,
    WireExit::Failure { error } if error == json!({"_tag": "NotFound", "id": -3})
  ));
  // Without a typed codec, or for a server stream: 415; garbage: 400.
  for uri in ["/rpc/Pc.json_only", "/rpc/Pc.count"] {
    let (status, content_type, _) = raw(pc(uri, Postcard::encode(&1_i64).unwrap())).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{uri}");
    assert_eq!(content_type, "application/problem+json");
  }
  // An output over the 64 KiB frame cap answers `ResourceExhausted`, still as postcard.
  let (status, _, body) = raw(pc("/rpc/Pc.big", Postcard::encode(&70_000_i64).unwrap())).await;
  assert_eq!(status, StatusCode::OK);
  assert!(matches!(
    decode_result(&body).unwrap().exit,
    WireExit::Failure { error } if error["_tag"] == "ResourceExhausted"
  ));
  let (status, ..) = raw(pc("/rpc", vec![0xff; 3])).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  let (status, ..) = raw(pc("/rpc/Pc.add", vec![0xff; 3])).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);

  // The Rust client over postcard, against the same app.
  let client = RpcClient::new("http://in.app");
  let env = hlist![HttpClientService(
    Arc::new(InApp(app.clone())) as Arc<dyn HttpClient>
  )];
  let runtime = id_effect::Runtime::global();
  let run = |eff: Effect<i64, RpcClientError, _>| {
    let eff = eff.provide_all(env.clone());
    async move { runtime.run(eff).await.into_result() }
  };
  let doubled = run(client.call_postcard("Pc.double", &8_i64)).await;
  let added = run(client.call_postcard("Pc.add", "eggs")).await;
  let missing = run(client.call_postcard("Pc.double", &-1_i64)).await;
  let refused = run(client.call_postcard("Pc.json_only", &1_i64)).await;
  assert_eq!(doubled.map_err(|c| format!("{c:?}")), Ok(16));
  assert_eq!(added.ok(), Some(2));
  let failure =
    |out: Result<i64, id_effect::Cause<RpcClientError>>| out.err().and_then(|c| c.fail_option());
  assert_eq!(failure(missing).unwrap().failure_tag(), Some("NotFound"));
  assert!(matches!(
    failure(refused),
    Some(RpcClientError::Refused(p)) if p.status == StatusCode::UNSUPPORTED_MEDIA_TYPE
  ));
  assert_eq!(adds.load(Ordering::SeqCst), 2);
}
