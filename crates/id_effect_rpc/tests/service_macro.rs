//! `#[rpc::service]` end to end (r12-e12-1-macro): the generated client against every kind,
//! the checked-in contract snapshot (D11), two versions side by side (web.rpc#6), guards before
//! handlers (C1), typed error unions (§4) and the HTTP transport.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use id_effect::{DynEnv, Effect, HNil, Never, Stream, fail, from_async, succeed};
use id_effect_axum::route::GuardFuture;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpClientService, HttpRequest, HttpResponse, StreamingHttpResponse,
};
use id_effect_rpc as rpc;
use rpc::contract::{Contract, ProcedureAccess, ProcedureKind, assert_snapshot};
use rpc::kinds::{Duplex, Subscription, SubscriptionItem};
use rpc::registry::RouteGuard;
use rpc::service::{
  BoxFuture, FrameStream, HttpTransport, LocalTransport, RpcCallError, RpcSchema, RpcService,
  RpcTransport, TransportCall, TransportError,
};
use rpc::wire::{CallId, Cursor, Frame, FrameKind, RpcFailure, WireExit};
use rpc::{RpcClient, RpcErrorStatus, RpcErrorUnion, WireSchema};
use serde_json::{Value, json};
use tower::ServiceExt;

// ---------------------------------------------------------------------------------------------
// The service under test
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, id_effect::Schema, serde::Serialize, serde::Deserialize)]
struct Todo {
  id: i64,
  title: String,
  done: bool,
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema, serde::Serialize, serde::Deserialize)]
struct NewTodo {
  title: String,
}

#[derive(Debug, PartialEq, rpc::RpcErrorUnion)]
enum TodoError {
  #[rpc(status = 404)]
  NotFound { id: i64 },
  #[rpc(status = 409)]
  Locked,
  #[rpc(tag = "TooLong")]
  Length { max: i64, note: Option<String> },
  #[rpc(internal)]
  Corrupt { detail: String },
}

/// Lets requests carrying `x-staff: yes` through.
struct Staff;

impl RouteGuard for Staff {
  fn policy(&self) -> &str {
    "staff"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let staff = parts.headers.get("x-staff").is_some_and(|v| v == "yes");
    Box::pin(async move {
      if staff {
        Ok(())
      } else {
        Err(StatusCode::FORBIDDEN.into_response())
      }
    })
  }
}

/// Keeps todos.
#[rpc::service(version = 1)]
trait Todos {
  /// One todo by id.
  #[query]
  #[public]
  fn get(&self, id: i64) -> Effect<Todo, TodoError, DynEnv>;

  /// How many todos there are.
  #[query]
  #[public]
  fn count(&self) -> Effect<i64, Never, DynEnv>;

  /// Adds a todo (staff only).
  #[mutation]
  #[guard(Staff)]
  fn add(&self, todo: NewTodo) -> Effect<Todo, TodoError, DynEnv>;

  /// 1, 2, …, n; a negative n fails after the stream started.
  #[server_stream]
  #[public]
  fn upto(&self, n: i64) -> Effect<Stream<i64, TodoError, DynEnv>, TodoError, DynEnv>;

  /// The sum of the client's numbers.
  #[client_stream(max_items = 100)]
  #[public]
  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv>;

  /// Every line back in upper case.
  #[bidi]
  #[public]
  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv>;

  /// Ticks from `from`, resumable.
  #[subscription]
  #[public]
  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> Effect<Subscription<i64, Never>, Never, DynEnv>;

  /// The titles, not resumable.
  #[subscription(name = "titles")]
  #[public]
  fn all_titles(&self) -> Effect<Subscription<String, Never>, Never, DynEnv>;
}

#[derive(Default)]
struct Store {
  added: Arc<AtomicUsize>,
}

impl Todos for Store {
  fn get(&self, id: i64) -> Effect<Todo, TodoError, DynEnv> {
    match id {
      1 => succeed(Todo {
        id,
        title: "milk".into(),
        done: false,
      }),
      2 => fail(TodoError::Locked),
      3 => fail(TodoError::Corrupt {
        detail: "secret".into(),
      }),
      4 => fail(TodoError::Length { max: 3, note: None }),
      _ => fail(TodoError::NotFound { id }),
    }
  }

  fn count(&self) -> Effect<i64, Never, DynEnv> {
    succeed(3)
  }

  fn add(&self, todo: NewTodo) -> Effect<Todo, TodoError, DynEnv> {
    self.added.fetch_add(1, Ordering::SeqCst);
    succeed(Todo {
      id: 9,
      title: todo.title,
      done: false,
    })
  }

  fn upto(&self, n: i64) -> Effect<Stream<i64, TodoError, DynEnv>, TodoError, DynEnv> {
    if n < 0 {
      succeed(Stream::from_vec(vec![1]).chain(Stream::fail(TodoError::NotFound { id: n })))
    } else {
      succeed(Stream::from_iterable(1..=n))
    }
  }

  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv> {
    items.run_fold(0, |a, b| a + b)
  }

  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv> {
    succeed(Duplex::new(|items: Stream<String, Never, DynEnv>| {
      items.map(|s| s.to_uppercase())
    }))
  }

  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> Effect<Subscription<i64, Never>, Never, DynEnv> {
    let start = after.map_or(from, |c| c.position as i64 + 1);
    let items = (start..from + 3)
      .map(|n| SubscriptionItem::new(n).with_cursor(Cursor::events(n as u64)))
      .collect();
    succeed(Subscription::new(Stream::from_vec(items)))
  }

  fn all_titles(&self) -> Effect<Subscription<String, Never>, Never, DynEnv> {
    succeed(Subscription::from_stream(Stream::from_vec(vec![
      "milk".to_owned(),
    ])))
  }
}

// ---------------------------------------------------------------------------------------------
// Two versions of one service
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct GreetIn {
  name: String,
  polite: bool,
}

/// Says hello.
#[rpc::service(version = 1, name = "Greeter")]
trait GreeterV1 {
  /// Greets someone.
  #[query]
  #[public]
  fn greet(&self, name: String) -> Effect<String, Never, DynEnv>;
}

/// Says hello, politely if asked.
#[rpc::service(version = 2, name = "Greeter")]
trait GreeterV2 {
  /// Greets someone.
  #[query]
  #[public]
  fn greet(&self, input: GreetIn) -> Effect<String, Never, DynEnv>;

  /// Every greeting so far.
  #[query]
  #[public]
  fn history(&self) -> Effect<Vec<String>, Never, DynEnv>;
}

struct Hello;

impl GreeterV1 for Hello {
  fn greet(&self, name: String) -> Effect<String, Never, DynEnv> {
    succeed(format!("Hello, {name}!"))
  }
}

impl GreeterV2 for Hello {
  fn greet(&self, input: GreetIn) -> Effect<String, Never, DynEnv> {
    let greeting = if input.polite { "Good day" } else { "Hi" };
    succeed(format!("{greeting}, {}!", input.name))
  }

  fn history(&self) -> Effect<Vec<String>, Never, DynEnv> {
    succeed(vec!["Hello, Ada!".to_owned()])
  }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

async fn run<A, E>(effect: Effect<A, E, HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  id_effect_axum::run_with_env(HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| cause.fail_option().expect("a typed failure"))
}

fn staff() -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert("x-staff", HeaderValue::from_static("yes"));
  headers
}

fn local(store: Store) -> LocalTransport {
  LocalTransport::new(TodosService::group(store), DynEnv::new()).with_headers(staff())
}

fn contract_document() -> Contract {
  TodosService::document()
    .with_service(GreeterV1Service::contract())
    .with_service(GreeterV2Service::contract())
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_client_round_trip_every_kind() {
  let client = TodosService::client(local(Store::default()));
  assert!(format!("{:?}", client.transport()).contains("procedures"));

  // query, mutation
  let milk = run(client.get(1)).await.unwrap();
  assert_eq!(milk.title, "milk");
  assert_eq!(run(client.count()).await.unwrap(), 3);
  let added = run(client.add(NewTodo {
    title: "eggs".into(),
  }))
  .await
  .unwrap();
  assert_eq!((added.id, added.title.as_str()), (9, "eggs"));

  // server stream, including a typed failure after the first item
  let upto = client.upto(3).flat_map(|items| items.run_collect());
  assert_eq!(run(upto).await.unwrap(), [1, 2, 3]);
  let failing = client.upto(-1).flat_map(|items| items.run_collect());
  assert_eq!(
    run(failing).await.unwrap_err().typed(),
    Some(&TodoError::NotFound { id: -1 })
  );

  // client stream
  assert_eq!(run(client.sum(vec![1, 2, 3])).await.unwrap(), 6);
  assert_eq!(run(client.sum(Vec::new())).await.unwrap(), 0);

  // bidi
  let shouted = client.shout().flat_map(|(tx, out)| {
    tx.send("a".to_owned())
      .flat_map(move |()| tx.send("b".to_owned()).map(move |()| drop(tx)))
      .flat_map(move |()| out.run_collect())
  });
  assert_eq!(run(shouted).await.unwrap(), ["A", "B"]);

  // subscription, then a resume after its first cursor
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
  let titles = run(client.all_titles(None).flat_map(|s| s.run_collect()))
    .await
    .unwrap();
  assert_eq!(titles, [SubscriptionItem::new("milk".to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_errors_cross_as_their_union_members() {
  let client = TodosService::client(local(Store::default()));
  let missing = run(client.get(7)).await.unwrap_err();
  assert_eq!(missing.typed(), Some(&TodoError::NotFound { id: 7 }));
  assert_eq!(
    run(client.get(2)).await.unwrap_err().typed(),
    Some(&TodoError::Locked)
  );
  assert_eq!(
    run(client.get(4)).await.unwrap_err().typed(),
    Some(&TodoError::Length { max: 3, note: None })
  );
  // An internal variant is a fault: the client only sees `InternalError { id }`.
  let corrupt = run(client.get(3)).await.unwrap_err();
  assert!(matches!(
    corrupt.failure(),
    Some(RpcFailure::InternalError { .. })
  ));
  assert!(!format!("{corrupt:?}").contains("secret"));

  // The derived union: statuses, tags, schema and decoding.
  assert_eq!(
    TodoError::NotFound { id: 1 }.status(),
    StatusCode::NOT_FOUND
  );
  assert_eq!(TodoError::Locked.status(), StatusCode::CONFLICT);
  assert_eq!(
    TodoError::Length {
      max: 1,
      note: Some("n".into())
    }
    .to_wire(),
    json!({"_tag": "TooLong", "max": 1, "note": "n"})
  );
  assert!(TodoError::Corrupt { detail: "x".into() }.is_internal());
  assert!(!TodoError::Locked.is_internal());
  let schema = TodoError::union_schema().unwrap();
  let tags: Vec<&Value> = schema["oneOf"]
    .as_array()
    .unwrap()
    .iter()
    .map(|v| &v["properties"]["_tag"]["const"])
    .collect();
  assert_eq!(
    tags,
    [&json!("NotFound"), &json!("Locked"), &json!("TooLong")]
  );
  assert_eq!(schema["oneOf"][2]["required"], json!(["_tag", "max"]));
  use rpc::service::RpcErrorDecode;
  assert_eq!(
    TodoError::from_wire(&json!({"_tag": "NotFound", "id": "x"})),
    None
  );
  assert_eq!(TodoError::from_wire(&json!({"_tag": "Corrupt"})), None);
  assert_eq!(TodoError::from_wire(&json!({"id": 1})), None);
}

#[test]
fn contract_snapshot_is_current() {
  let document = contract_document();
  assert_snapshot(&document, "rpc.contract.json");

  let todos = TodosService::contract();
  assert_eq!(todos.description.as_deref(), Some("Keeps todos."));
  assert_eq!(
    (TodosService::NAME, <TodosService as RpcService>::VERSION),
    ("Todos", 1)
  );
  let names: Vec<_> = todos.procedures.iter().map(|p| p.name.as_str()).collect();
  assert_eq!(
    names,
    [
      "add", "count", "get", "shout", "sum", "ticks", "titles", "upto"
    ]
  );
  let add = todos.procedure("add").unwrap();
  assert_eq!(add.kind, ProcedureKind::Mutation);
  assert!(add.idempotent);
  assert_eq!(
    add.access,
    ProcedureAccess::Guarded {
      policies: vec!["staff".into()]
    }
  );
  assert_eq!(todos.procedure("sum").unwrap().max_items, Some(100));
  assert_eq!(todos.procedure("count").unwrap().error, None);
  assert_eq!(todos.procedure("get").unwrap().input["type"], "integer");
  // The contract is what a registration serves.
  let served = TodosService::group(Store::default()).service_contract("Todos", 1);
  assert_eq!(served.procedures, todos.procedures);
  assert_eq!(<TodosService as RpcService>::contract(), todos);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_versions_served_side_by_side() {
  let mut group = rpc::RpcGroup::new();
  GreeterV1Service::register(&mut group, Hello);
  GreeterV2Service::register(&mut group, Hello);
  assert_eq!(group.get("Greeter.greet").unwrap().version(), 2);
  assert_eq!(group.get_version("Greeter.greet", 1).unwrap().version(), 1);
  let transport = LocalTransport::new(group, DynEnv::new());
  let v1 = GreeterV1Service::client(transport.clone());
  let v2 = GreeterV2Service::client(transport.clone());
  assert_eq!(GreeterV1Client::<LocalTransport>::VERSION, 1);
  assert_eq!(run(v1.greet("Ada".into())).await.unwrap(), "Hello, Ada!");
  let polite = GreetIn {
    name: "Ada".into(),
    polite: true,
  };
  assert_eq!(run(v2.greet(polite)).await.unwrap(), "Good day, Ada!");
  assert_eq!(run(v2.history()).await.unwrap(), ["Hello, Ada!"]);
  let document = contract_document();
  let versions: Vec<u32> = document
    .services
    .iter()
    .filter(|s| s.name == "Greeter")
    .map(|s| s.version)
    .collect();
  assert_eq!(versions, [1, 2]);

  // Only version 1 served: version 2's procedures are unknown (404) to its client.
  let only_v1 = LocalTransport::new(GreeterV1Service::group(Hello), DynEnv::new());
  let err = run(GreeterV2Service::client(only_v1).history())
    .await
    .unwrap_err();
  assert!(matches!(
    err,
    RpcCallError::Transport(TransportError::Refused { status: 404, .. })
  ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guarded_procedure_runs_guard_before_handler() {
  let store = Store::default();
  let added = store.added.clone();
  let group = TodosService::group(store);
  let anonymous = TodosService::client(LocalTransport::new(group.clone(), DynEnv::new()));
  let refused = run(anonymous.add(NewTodo { title: "x".into() }))
    .await
    .unwrap_err();
  assert!(matches!(
    refused,
    RpcCallError::Transport(TransportError::Refused { status: 403, .. })
  ));
  assert_eq!(added.load(Ordering::SeqCst), 0, "the handler never ran");
  // Public procedures stay open to the same caller.
  assert_eq!(run(anonymous.count()).await.unwrap(), 3);

  let staff_client =
    TodosService::client(LocalTransport::new(group, DynEnv::new()).with_headers(staff()));
  run(staff_client.add(NewTodo { title: "x".into() }))
    .await
    .unwrap();
  assert_eq!(added.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------------------------
// HTTP transport over the mounted router (no network)
// ---------------------------------------------------------------------------------------------

struct AxumHttp {
  app: axum::Router,
  down: bool,
}

impl HttpClient for AxumHttp {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    if self.down {
      return fail(HttpError::InvalidRequest("connection refused".into()));
    }
    let app = self.app.clone();
    from_async(move || async move {
      let mut builder = axum::http::Request::post(&req.url);
      for (name, value) in &req.headers {
        builder = builder.header(name, value);
      }
      let request = builder
        .body(axum::body::Body::from(req.body.unwrap_or_default()))
        .expect("request");
      let response = app.oneshot(request).await.expect("infallible");
      let status = response.status().as_u16();
      let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
      Ok(HttpResponse {
        status,
        headers: vec![],
        body: body.to_vec(),
      })
    })
  }

  fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    fail(HttpError::InvalidRequest("not streamed".into()))
  }
}

fn http(down: bool, headers: bool) -> HttpTransport {
  let router = TodosService::mount(id_effect_axum::route::Router::new(), Store::default());
  let app: axum::Router = router.with_state(DynEnv::new()).into_axum();
  let app = if headers {
    app.layer(axum::middleware::from_fn(
      |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
        req
          .headers_mut()
          .insert("x-staff", HeaderValue::from_static("yes"));
        next.run(req).await
      },
    ))
  } else {
    app
  };
  HttpTransport::new(
    RpcClient::new("http://rpc.test"),
    HttpClientService::new(AxumHttp { app, down }),
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_transport_carries_queries_and_mutations() {
  let client = TodosService::client(http(false, true));
  assert!(format!("{:?}", client.transport()).contains("http://rpc.test/rpc"));
  assert_eq!(run(client.get(1)).await.unwrap().title, "milk");
  assert_eq!(
    run(client.get(8)).await.unwrap_err().typed(),
    Some(&TodoError::NotFound { id: 8 })
  );
  assert_eq!(
    run(client.add(NewTodo { title: "y".into() }))
      .await
      .unwrap()
      .id,
    9
  );
  let stream = run(client.upto(2)).await.map(|_| ()).unwrap_err();
  assert!(matches!(
    stream,
    RpcCallError::Transport(TransportError::Unsupported(ProcedureKind::ServerStream))
  ));

  let anonymous = TodosService::client(http(false, false));
  let refused = run(anonymous.add(NewTodo { title: "y".into() }))
    .await
    .unwrap_err();
  assert!(matches!(
    refused,
    RpcCallError::Transport(TransportError::Refused { status: 403, .. })
  ));
  let down = TodosService::client(http(true, false));
  let err = run(down.count()).await.unwrap_err();
  assert!(err.to_string().contains("connection refused"));
}

/// Every request body a [`Recording`] client sent.
type Bodies = Arc<std::sync::Mutex<Vec<Value>>>;

/// Every request's headers a [`Recording`] client sent.
type SentHeaders = Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>;

/// [`AxumHttp`] that keeps every request body and header list it sends.
struct Recording {
  inner: AxumHttp,
  bodies: Bodies,
  headers: SentHeaders,
}

impl HttpClient for Recording {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    let body = serde_json::from_slice(req.body.as_deref().unwrap_or_default()).expect("json");
    self.bodies.lock().unwrap().push(body);
    self.headers.lock().unwrap().push(req.headers.clone());
    self.inner.execute(req)
  }

  fn execute_stream(&self, req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    self.inner.execute_stream(req)
  }
}

/// An [`HttpTransport`] over a recording client; `staff` adds `x-staff: yes` server side.
fn recording(staff: bool) -> (HttpTransport, Bodies, SentHeaders) {
  let bodies = Bodies::default();
  let headers = SentHeaders::default();
  let router = TodosService::mount(id_effect_axum::route::Router::new(), Store::default());
  let app: axum::Router = router.with_state(DynEnv::new()).into_axum();
  let app = if staff {
    app.layer(axum::middleware::from_fn(
      |mut req: axum::extract::Request, next: axum::middleware::Next| async move {
        req
          .headers_mut()
          .insert("x-staff", HeaderValue::from_static("yes"));
        next.run(req).await
      },
    ))
  } else {
    app
  };
  let http = Recording {
    inner: AxumHttp { app, down: false },
    bodies: bodies.clone(),
    headers: headers.clone(),
  };
  let transport = HttpTransport::new(
    RpcClient::new("http://rpc.test"),
    HttpClientService::new(http),
  );
  (transport, bodies, headers)
}

fn recorded() -> (TodosClient<HttpTransport>, Bodies) {
  let (transport, bodies, _) = recording(true);
  (TodosService::client(transport), bodies)
}

// One worker: same-tick batching is deterministic on a FIFO executor and only timing-dependent
// on a multi-thread one (id_effect::request, "The batch window").
#[tokio::test(flavor = "current_thread")]
async fn generated_client_batches_same_tick_calls() {
  let (client, bodies) = recorded();
  let both = id_effect::all(
    vec![
      client.get(1).map(|t| t.title),
      client.add(NewTodo { title: "z".into() }).map(|t| t.title),
    ],
    id_effect::Concurrency::Unbounded,
  );
  assert_eq!(run(both).await.unwrap(), ["milk", "z"]);
  let sent = bodies.lock().unwrap().clone();
  assert_eq!(sent.len(), 1, "one request: {sent:?}");
  let calls = sent[0].as_array().expect("a batch array");
  let mut tags: Vec<_> = calls
    .iter()
    .map(|c| (c["tag"].to_string(), c["v"].clone()))
    .collect();
  tags.sort_by(|a, b| a.0.cmp(&b.0));
  assert_eq!(
    tags,
    [
      ("\"Todos.add\"".to_owned(), json!(1)),
      ("\"Todos.get\"".to_owned(), json!(1))
    ]
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_client_lone_call_is_plain_envelope() {
  let (client, bodies) = recorded();
  assert_eq!(
    run(client.get(8)).await.unwrap_err().typed(),
    Some(&TodoError::NotFound { id: 8 })
  );
  let sent = bodies.lock().unwrap().clone();
  assert_eq!(sent.len(), 1);
  assert_eq!(sent[0]["tag"], "Todos.get");
  assert_eq!(sent[0]["v"], 1);
  assert_eq!(sent[0]["payload"], 8);
}

/// The values of `name` in one request's headers.
fn values<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
  headers
    .iter()
    .filter(|(n, _)| n.eq_ignore_ascii_case(name))
    .map(|(_, v)| v.as_str())
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_transport_attaches_bearer() {
  // No staff middleware: the guard sees `x-staff` only because the transport sends it.
  let (transport, bodies, headers) = recording(false);
  let transport = transport
    .with_header("X-Staff", "yes")
    .unwrap()
    .with_bearer(id_effect_config::Secret::new("tok-123".to_owned()))
    .unwrap();
  let client = TodosService::client(transport.clone());
  let both = id_effect::all(
    vec![
      client.get(1).map(|t| t.title),
      client.add(NewTodo { title: "z".into() }).map(|t| t.title),
    ],
    id_effect::Concurrency::Unbounded,
  );
  assert_eq!(run(both).await.unwrap(), ["milk", "z"]);
  // The unbatched path (`RpcTransport::unary`) carries them too.
  let call = TransportCall {
    tag: "Todos.add".to_owned(),
    version: 1,
    kind: ProcedureKind::Mutation,
    payload: json!({"title": "q"}),
  };
  assert!(matches!(
    transport.unary(call).await,
    Ok(WireExit::Success { .. })
  ));
  let sent = headers.lock().unwrap().clone();
  assert_eq!(bodies.lock().unwrap().len(), sent.len());
  assert!(!sent.is_empty());
  for request in &sent {
    assert_eq!(values(request, "authorization"), ["Bearer tok-123"]);
    assert_eq!(values(request, "x-staff"), ["yes"]);
    assert_eq!(values(request, "x-rpc"), ["1"]);
    assert_eq!(values(request, "content-type"), ["application/json"]);
  }
}

#[test]
fn http_transport_rejects_invalid_header() {
  use rpc::service::HttpHeaderError;
  let (transport, ..) = recording(false);
  let header = |name: &str, value: &str| transport.clone().with_header(name, value).map(|_| ());
  for name in ["", "bad header", "x:y", "x\r\ny", "café"] {
    assert_eq!(
      header(name, "v"),
      Err(HttpHeaderError::InvalidName(name.to_owned())),
      "{name:?}"
    );
  }
  for value in ["a\r\nx-rpc: 0", "a\nb", "nul\0", "del\u{7f}", "café"] {
    let err = header("X-Tenant", value).unwrap_err();
    assert_eq!(
      err,
      HttpHeaderError::InvalidValue {
        name: "x-tenant".to_owned()
      }
    );
    assert!(!err.to_string().contains(value), "{err}");
  }
  assert!(header("x-tenant", "acme corp\tA-1").is_ok());
  let bearer = |token: &str| {
    transport
      .clone()
      .with_bearer(id_effect_config::Secret::new(token.to_owned()))
      .map(|_| ())
  };
  for token in ["", "tok\r\nx-rpc: 0"] {
    let err = bearer(token).unwrap_err();
    assert_eq!(
      err,
      HttpHeaderError::InvalidValue {
        name: "authorization".to_owned()
      }
    );
    assert!(!err.to_string().contains("tok"));
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_transport_cannot_override_rpc_header() {
  use rpc::service::HttpHeaderError;
  let (transport, _, headers) = recording(true);
  for name in ["x-rpc", "X-RPC", "Content-Type", "content-length", "Host"] {
    assert_eq!(
      transport.clone().with_header(name, "0").map(|_| ()),
      Err(HttpHeaderError::Reserved(name.to_ascii_lowercase())),
      "{name}"
    );
  }
  assert!(
    HttpHeaderError::Reserved("x-rpc".to_owned())
      .to_string()
      .contains("reserved")
  );
  // A later value of a name replaces the earlier one; the CSRF header stays the transport's.
  let transport = transport
    .with_header("x-tenant", "a")
    .unwrap()
    .with_header("X-Tenant", "b")
    .unwrap();
  let client = TodosService::client(transport);
  assert_eq!(
    run(client.add(NewTodo { title: "y".into() }))
      .await
      .unwrap()
      .id,
    9
  );
  let sent = headers.lock().unwrap().clone();
  assert_eq!(sent.len(), 1);
  assert_eq!(values(&sent[0], "x-rpc"), ["1"]);
  assert_eq!(values(&sent[0], "content-type"), ["application/json"]);
  assert_eq!(values(&sent[0], "x-tenant"), ["b"]);
}

#[test]
fn http_transport_bearer_redacted_in_debug() {
  let (transport, ..) = recording(false);
  let transport = transport
    .with_header("x-api-key", "plain-key-42")
    .unwrap()
    .with_bearer(id_effect_config::Secret::new("tok-very-secret".to_owned()))
    .unwrap();
  let shown = format!("{transport:?}");
  assert!(!shown.contains("tok-very-secret"), "{shown}");
  assert!(!shown.contains("plain-key-42"), "{shown}");
  assert!(shown.contains("authorization") && shown.contains("x-api-key"));
  let client = TodosService::client(transport);
  assert!(!format!("{:#?}", client.transport()).contains("tok-very-secret"));
}

// ---------------------------------------------------------------------------------------------
// Wire faults on the client side
// ---------------------------------------------------------------------------------------------

/// Encodes to nothing: every call that sends one fails before it leaves.
struct Unsendable;

impl RpcSchema for Unsendable {
  fn wire_schema() -> WireSchema<Self> {
    WireSchema::from_fns(|_| Ok(Unsendable), |_| Err("unsendable".into()), json!({}))
  }
}

/// Encodes as `1` but never decodes: an item the client cannot read.
struct Unreadable;

impl RpcSchema for Unreadable {
  fn wire_schema() -> WireSchema<Self> {
    WireSchema::from_fns(
      |_| {
        Err(vec![rpc::wire::InputIssue {
          path: String::new(),
          message: "unreadable".into(),
        }])
      },
      |_| Ok(json!(1)),
      json!({}),
    )
  }
}

#[rpc::service(version = 1)]
trait Faulty {
  #[mutation]
  #[public]
  fn send(&self, input: Unsendable) -> Effect<(), Never, DynEnv>;

  #[server_stream]
  #[public]
  fn stream(&self, input: Unsendable) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;

  #[server_stream]
  #[public]
  fn unreadable(&self) -> Effect<Stream<Unreadable, Never, DynEnv>, Never, DynEnv>;

  #[client_stream]
  #[public]
  fn collect(&self, items: Stream<Unsendable, Never, DynEnv>) -> Effect<(), Never, DynEnv>;

  #[bidi]
  #[public]
  fn talk(&self) -> Effect<Duplex<Unsendable, i64, Never>, Never, DynEnv>;

  #[bidi]
  #[public]
  fn chat(&self) -> Effect<Duplex<i64, i64, Never>, Never, DynEnv>;

  #[subscription]
  #[public]
  fn watch(&self, input: Unsendable) -> Effect<Subscription<i64, Never>, Never, DynEnv>;
}

struct Faults;

impl Faulty for Faults {
  fn send(&self, _input: Unsendable) -> Effect<(), Never, DynEnv> {
    succeed(())
  }

  fn stream(&self, _input: Unsendable) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::empty())
  }

  fn unreadable(&self) -> Effect<Stream<Unreadable, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::from_vec(vec![Unreadable]))
  }

  fn collect(&self, items: Stream<Unsendable, Never, DynEnv>) -> Effect<(), Never, DynEnv> {
    items.run_drain()
  }

  fn talk(&self) -> Effect<Duplex<Unsendable, i64, Never>, Never, DynEnv> {
    succeed(Duplex::new(|_items| Stream::empty()))
  }

  fn chat(&self) -> Effect<Duplex<i64, i64, Never>, Never, DynEnv> {
    succeed(Duplex::new(|_items| Stream::from_vec(vec![1])))
  }

  fn watch(&self, _input: Unsendable) -> Effect<Subscription<i64, Never>, Never, DynEnv> {
    succeed(Subscription::from_stream(Stream::empty()))
  }
}

fn encode_failed<A>(result: Result<A, RpcCallError<Never>>) -> bool {
  matches!(result, Err(RpcCallError::Encode(ref why)) if why == "unsendable")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inputs_that_do_not_encode_and_items_that_do_not_decode_fail_on_the_client() {
  let client = FaultyService::client(LocalTransport::new(
    FaultyService::group(Faults),
    DynEnv::new(),
  ));
  assert!(encode_failed(run(client.send(Unsendable)).await));
  assert!(encode_failed(
    run(client.stream(Unsendable)).await.map(|_| ())
  ));
  assert!(encode_failed(run(client.collect(vec![Unsendable])).await));
  assert!(encode_failed(
    run(client.watch(Unsendable, None)).await.map(|_| ())
  ));
  let sent = client
    .talk()
    .flat_map(|(tx, out)| tx.send(Unsendable).map(move |()| drop(out)));
  assert!(encode_failed(run(sent).await));
  let unreadable = run(client.unreadable().flat_map(|s| s.run_collect()))
    .await
    .map(|_| ())
    .unwrap_err();
  assert!(
    matches!(unreadable, RpcCallError::Decode(ref issues) if issues[0].message == "unreadable")
  );

  // A closed two-way stream refuses further items.
  let closed = client
    .chat()
    .flat_map(|(tx, out)| out.run_collect().flat_map(move |_| tx.send(5)));
  let closed = run(closed).await.unwrap_err();
  assert!(closed.to_string().contains("closed"), "{closed}");
  assert!(format!("{:?}", run(client.chat()).await.unwrap().0).contains("BidiSender"));
  assert!(format!("{:?}", FaultyService).contains("FaultyService"));
}

/// Serves canned frames to every stream call.
#[derive(Clone)]
struct Canned(Vec<Frame>);

impl RpcTransport for Canned {
  fn unary(&self, _call: TransportCall) -> BoxFuture<Result<WireExit, TransportError>> {
    Box::pin(async {
      Ok(WireExit::Success {
        value: json!("not a number"),
      })
    })
  }

  fn server_stream(&self, _call: TransportCall) -> BoxFuture<Result<FrameStream, TransportError>> {
    let frames = self.0.clone();
    Box::pin(async move { Ok(Box::pin(futures_util::stream::iter(frames)) as FrameStream) })
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_frames_are_protocol_errors() {
  let id = CallId::new(1).unwrap();
  let item = Frame::new(id, FrameKind::Item).with_payload(json!(1));
  let credit = Frame::new(id, FrameKind::Credit);
  let bad_exit = Frame::new(id, FrameKind::Exit).with_payload(json!({"_tag": "Nope"}));
  let collect = |frames: Vec<Frame>| {
    let client = TodosService::client(Canned(frames));
    async move { run(client.upto(1).flat_map(|s| s.run_collect())).await }
  };
  let cut = collect(vec![item.clone(), credit]).await.unwrap_err();
  assert!(cut.to_string().contains("without an exit frame"));
  let bad = collect(vec![item, bad_exit]).await.unwrap_err();
  assert!(matches!(
    bad,
    RpcCallError::Transport(TransportError::Protocol(_))
  ));
  let unreadable = run(TodosService::client(Canned(vec![])).count())
    .await
    .unwrap_err();
  assert!(matches!(unreadable, RpcCallError::Decode(_)));
  let unsupported = run(TodosService::client(Canned(vec![])).sum(vec![1]))
    .await
    .unwrap_err();
  assert!(matches!(
    unsupported,
    RpcCallError::Transport(TransportError::Unsupported(ProcedureKind::ClientStream))
  ));
}

// ---------------------------------------------------------------------------------------------
// Postcard (r12-e12-1-postcard): serde types get a typed bridge, error unions stay JSON
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "postcard")]
mod postcard {
  use super::*;
  use rpc::cancel::CallControl;
  use rpc::codec::postcard::{
    Postcard, decode_client_payload, decode_frame, decode_result, encode_frame, encode_result,
  };
  use rpc::codec::{Codec, CodecId};
  use rpc::handler::CallContext;
  use rpc::server::dispatch;
  use rpc::wire::WireResult;

  /// A postcard `get(id)` call through the procedure's bridge, as a postcard client reads it.
  async fn get(group: &rpc::RpcGroup, id: i64) -> WireExit<Vec<u8>, Value> {
    let mut frame = Frame::new(CallId::new(5).unwrap(), FrameKind::Call);
    frame.tag = Some("Todos.get".to_owned());
    frame.payload = Some(Postcard::encode(&id).unwrap());
    let frame = decode_frame(&encode_frame(&frame).unwrap()).unwrap();
    let entry = dispatch::resolve(group, frame.tag.as_deref().unwrap(), frame.v).unwrap();
    let input = decode_client_payload(entry.codecs(), frame.payload.as_deref()).unwrap();
    let ctx = CallContext::new(DynEnv::new(), "pc");
    let outcome = dispatch::start_unary(entry, input, ctx, CallControl::none())
      .unwrap()
      .await;
    let exit = entry
      .codecs()
      .encode_exit(CodecId::Postcard, &outcome.exit)
      .unwrap();
    let bytes = encode_result(&WireResult::new(frame.id.into(), "Todos.get", exit)).unwrap();
    decode_result(&bytes).unwrap().exit
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn generated_service_answers_postcard_calls() {
    let group = TodosService::group(Store::default());
    let entry = group.get("Todos.get").unwrap();
    assert!(entry.codecs().input().is_some() && entry.codecs().output().is_some());
    let WireExit::Success { value } = get(&group, 1).await else {
      panic!("expected a success");
    };
    let todo: Todo = Postcard::decode(&value).unwrap();
    assert_eq!(todo.title, "milk");
    // The typed error stays JSON inside the postcard exit.
    let WireExit::Failure { error } = get(&group, 7).await else {
      panic!("expected a failure");
    };
    assert_eq!(error, json!({"_tag": "NotFound", "id": 7}));

    // Every kind of a serde service is bridged; a type without serde stays JSON-only.
    for tag in [
      "Todos.add",
      "Todos.upto",
      "Todos.sum",
      "Todos.shout",
      "Todos.ticks",
    ] {
      let codecs = group.get(tag).unwrap().codecs();
      assert!(
        codecs.input().is_some() && codecs.output().is_some(),
        "{tag}"
      );
    }
    let faulty = FaultyService::group(Faults);
    assert!(
      faulty
        .get("Faulty.send")
        .unwrap()
        .codecs()
        .input()
        .is_none()
    );
  }
}
