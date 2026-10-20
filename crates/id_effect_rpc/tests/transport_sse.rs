//! Subscriptions and server streams as server-sent events next to the HTTP routes (design note
//! §2): `GET /rpc/{tag}` with `Accept: text/event-stream`, cursors as event ids, `Last-Event-ID`
//! resume, mutations over `POST` beside it, and the Rust SSE client.
#![cfg(all(feature = "sse", feature = "client"))]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use id_effect::{DynEnv, Effect, Exit, HNil, Never, Stream, from_async, schema, succeed};
use id_effect_axum::EffectExecutor;
use id_effect_axum::realtime::sse::SseConfig;
use id_effect_axum::route::{GuardFuture, RouteGuard};
use id_effect_platform::http::{
  HttpClient, HttpClientConfig, HttpClientService, HttpRequest, ReqwestHttpClient,
};
use id_effect_rpc::client::{CallEvent, RpcClient, RpcClientError, TransportKind};
use id_effect_rpc::kinds::{Subscription, SubscriptionItem};
use id_effect_rpc::wire::Cursor;
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt as _;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

struct Deny;

impl RouteGuard for Deny {
  fn policy(&self) -> &str {
    "deny"
  }

  fn check<'a>(&'a self, _: &'a Parts) -> GuardFuture<'a> {
    Box::pin(async {
      Err(axum::response::IntoResponse::into_response(
        StatusCode::FORBIDDEN,
      ))
    })
  }
}

fn server(posts: broadcast::Sender<String>) -> RpcServer {
  let mut group = RpcGroup::new();
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
  let live = posts.clone();
  group.subscription(
    Procedure::subscription("Wall.live").public(),
    text(),
    text(),
    move |_: String, _, _: &mut DynEnv| {
      let rx = Arc::new(tokio::sync::Mutex::new(live.subscribe()));
      let stream = Stream::from_pull(move |_| {
        let rx = rx.clone();
        from_async(move || async move {
          Ok::<_, Never>(rx.lock().await.recv().await.ok().map(|m| vec![m]))
        })
      });
      succeed::<_, Never, _>(Subscription::from_stream(stream))
    },
  );
  group.mutation(
    Procedure::mutation("Wall.post").public(),
    text(),
    int(),
    move |text: String, _: &mut DynEnv| {
      let n = posts.send(text).unwrap_or(0);
      succeed::<_, Never, _>(n as i64)
    },
  );
  group.server_stream(
    Procedure::server_stream("Count.up").public(),
    int(),
    int(),
    |n: i64, _: &mut DynEnv| succeed::<_, Never, _>(Stream::from_iterable(1..=n)),
  );
  group.subscription(
    Procedure::subscription("Secret.watch").guard(Deny),
    text(),
    text(),
    |_: String, _, _: &mut DynEnv| {
      succeed::<_, Never, _>(Subscription::from_stream(Stream::empty()))
    },
  );
  RpcServer::new(group)
}

fn app(server: RpcServer) -> axum::Router {
  server.router().with_state(DynEnv::new()).into_axum()
}

async fn get(app: &axum::Router, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
  let mut req = Request::get(uri).header("accept", "text/event-stream");
  for (k, v) in headers {
    req = req.header(*k, *v);
  }
  let res = app
    .clone()
    .oneshot(req.body(Body::empty()).unwrap())
    .await
    .unwrap();
  let status = res.status();
  if status == StatusCode::OK {
    assert_eq!(res.headers()["content-type"], "text/event-stream");
  }
  let bytes = res.into_body().collect().await.unwrap().to_bytes();
  (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_over_sse() {
  let app = app(server(broadcast::channel(8).0));
  let (status, body) = get(&app, "/rpc/Feed.watch?input=%22a%22", &[]).await;
  assert_eq!(status, StatusCode::OK);
  let expected = format!(
    "event: item\ndata: \"a#0\"\nid: {}\n\nevent: item\ndata: \"a#1\"\nid: {}\n\nevent: item\ndata: \"a#2\"\nid: {}\n\nevent: exit\ndata: {{\"_tag\":\"Success\",\"value\":null}}\n\n",
    Cursor::events(0),
    Cursor::events(1),
    Cursor::events(2)
  );
  assert_eq!(body, expected);

  // A reconnecting EventSource sends the last id back and resumes after it.
  let last = Cursor::events(1).to_string();
  let (status, body) = get(
    &app,
    "/rpc/Feed.watch?input=%22a%22",
    &[("last-event-id", &last)],
  )
  .await;
  assert_eq!(status, StatusCode::OK);
  assert!(body.starts_with("event: item\ndata: \"a#2\""), "{body}");

  // A Last-Event-ID that is not a cursor, or not a valid header, is a 400 Problem.
  let (status, body) = get(
    &app,
    "/rpc/Feed.watch?input=%22a%22",
    &[("last-event-id", "7")],
  )
  .await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  assert!(body.contains("cursor"), "{body}");
  let long = "x".repeat(300);
  let (status, _) = get(
    &app,
    "/rpc/Feed.watch?input=%22a%22",
    &[("last-event-id", &long)],
  )
  .await;
  assert_eq!(status, StatusCode::BAD_REQUEST);

  // Input that does not decode is the call's InvalidInput exit; input that is not JSON a 400.
  let (status, body) = get(&app, "/rpc/Feed.watch?input=1", &[]).await;
  assert_eq!(status, StatusCode::OK);
  assert!(
    body.contains("event: exit") && body.contains("InvalidInput"),
    "{body}"
  );
  let (status, _) = get(&app, "/rpc/Feed.watch?input=%7B", &[]).await;
  assert_eq!(status, StatusCode::BAD_REQUEST);

  // Guards run on the GET; unknown tags are 404; without the Accept header it stays 406.
  let (status, _) = get(&app, "/rpc/Secret.watch?input=%22a%22", &[]).await;
  assert_eq!(status, StatusCode::FORBIDDEN);
  let (status, _) = get(&app, "/rpc/Nope.watch", &[]).await;
  assert_eq!(status, StatusCode::NOT_FOUND);
  let plain = app
    .clone()
    .oneshot(Request::get("/rpc/Feed.watch").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(plain.status(), StatusCode::NOT_ACCEPTABLE);

  // Server streams too (they ignore Last-Event-ID).
  let (status, body) = get(&app, "/rpc/Count.up?input=2", &[("last-event-id", "junk")]).await;
  assert_eq!(status, StatusCode::OK);
  assert_eq!(
    body,
    "event: item\ndata: 1\n\nevent: item\ndata: 2\n\nevent: exit\ndata: {\"_tag\":\"Success\",\"value\":null}\n\n"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_streams_are_capped_per_client() {
  let posts = broadcast::channel(8).0;
  let app = app(server(posts).with_sse(SseConfig::default().max_streams(1, 1)));
  let open = app
    .clone()
    .oneshot(
      Request::get("/rpc/Wall.live?input=%22a%22")
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(open.status(), StatusCode::OK);
  let (status, _) = get(&app, "/rpc/Wall.live?input=%22b%22", &[]).await;
  assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
  // Dropping the response releases the slot once the stream's fiber has stopped.
  drop(open);
  let mut status = StatusCode::TOO_MANY_REQUESTS;
  for _ in 0..100 {
    status = get(&app, "/rpc/Count.up?input=1", &[]).await.0;
    if status == StatusCode::OK {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert_eq!(status, StatusCode::OK);
}

async fn listen(server: RpcServer) -> SocketAddr {
  let app = app(server);
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

fn http() -> HttpClientService {
  let config = HttpClientConfig::default().allow_hosts(["127.0.0.1"]);
  HttpClientService(Arc::new(ReqwestHttpClient::from_config(&config)) as Arc<dyn HttpClient>)
}

async fn run<A: Send + 'static>(
  effect: Effect<A, RpcClientError, HNil>,
) -> Result<A, RpcClientError> {
  match EffectExecutor::global().run_effect(effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(cause.fail_option().expect("a typed failure")),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutation_over_post_next_to_sse() {
  let posts = broadcast::channel(8).0;
  let addr = listen(server(posts.clone())).await;
  let client = RpcClient::new(format!("http://{addr}")).with_transport(TransportKind::Sse);
  let env = id_effect::hlist![http()];
  let stream = run(
    client
      .subscribe("Wall.live", json!("w"), None)
      .provide_all(env.clone()),
  )
  .await
  .unwrap();
  for _ in 0..100 {
    if posts.receiver_count() > 0 {
      break;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  assert_eq!(posts.receiver_count(), 1, "the subscription is live");
  // The mutation goes over POST, beside the event stream.
  let post = HttpRequest {
    headers: vec![
      ("content-type".into(), "application/json".into()),
      ("x-rpc".into(), "1".into()),
    ],
    ..HttpRequest::post(
      format!("http://{addr}/rpc/Wall.post"),
      b"\"hello\"".to_vec(),
    )
  };
  let response = http().0.execute(post).provide_all(HNil);
  let response = match EffectExecutor::global().run_effect(response).await {
    Exit::Success(r) => r,
    Exit::Failure(cause) => panic!("{cause:?}"),
  };
  assert_eq!(response.status, 200);
  let event = run(stream.next()).await.unwrap();
  assert!(
    matches!(&event, Some(CallEvent::Item { value, cursor: None }) if value == "hello"),
    "{event:?}"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sse_client_reads_resumes_and_reports_refusals() {
  let addr = listen(server(broadcast::channel(8).0)).await;
  let client = RpcClient::new(format!("http://{addr}"));
  let env = id_effect::hlist![http()];
  let call = run(
    client
      .sse("Feed.watch", &json!("r"), Some(Cursor::events(4)))
      .provide_all(env.clone()),
  )
  .await
  .unwrap();
  assert_eq!(call.last_cursor(), Some(Cursor::events(4)));
  let mut values = Vec::new();
  while let Some(event) = run(call.next()).await.unwrap() {
    match event {
      CallEvent::Item { value, .. } => values.push(value),
      CallEvent::Exit(result) => assert_eq!(result.unwrap(), Value::Null),
    }
  }
  assert_eq!(values, [json!("r#5"), json!("r#6"), json!("r#7")]);
  assert_eq!(call.last_cursor(), Some(Cursor::events(7)));
  assert!(format!("{call:?}").contains("SseCall"));

  let refused = run(
    client
      .sse("Secret.watch", &json!("r"), None)
      .provide_all(env.clone()),
  )
  .await
  .unwrap_err();
  assert!(
    matches!(
      &refused,
      RpcClientError::UnexpectedStatus(403) | RpcClientError::Refused(_)
    ),
    "{refused:?}"
  );
  let unknown = run(
    client
      .sse("Nope.watch", &json!(1), None)
      .provide_all(env.clone()),
  )
  .await
  .unwrap_err();
  assert!(
    matches!(&unknown, RpcClientError::Refused(p) if p.status == StatusCode::NOT_FOUND),
    "{unknown:?}"
  );
  let down = RpcClient::new("http://127.0.0.1:1");
  let err = run(down.sse("A.b", &Value::Null, None).provide_all(env))
    .await
    .unwrap_err();
  assert!(matches!(err, RpcClientError::Http(_)));
}

// ---------------------------------------------------------------------------------------------
// A generated client (`#[rpc::service]`) over HTTP and server-sent events
// ---------------------------------------------------------------------------------------------

/// Counts.
#[id_effect_rpc::service(version = 1)]
trait Counter {
  /// Twice `n`.
  #[query]
  #[public]
  fn twice(&self, n: i64) -> Effect<i64, Never, DynEnv>;

  /// 1 to `n`.
  #[server_stream]
  #[public]
  fn upto(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;

  /// The sum of the items.
  #[client_stream]
  #[public]
  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv>;

  /// Three ticks from `from`, resumable.
  #[subscription]
  #[public]
  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> Effect<Subscription<i64, Never>, Never, DynEnv>;
}

struct Count;

impl Counter for Count {
  fn twice(&self, n: i64) -> Effect<i64, Never, DynEnv> {
    succeed(n * 2)
  }

  fn upto(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::from_iterable(1..=n))
  }

  fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv> {
    items.run_fold(0, |a, b| a + b)
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
}

async fn run_any<A, E>(effect: Effect<A, E, HNil>) -> Result<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  match EffectExecutor::global().run_effect(effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(cause) => Err(cause.fail_option().expect("a typed failure")),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_clients_stream_over_sse() {
  use id_effect_rpc::client::sse::SseTransport;
  let addr = listen(RpcServer::new(CounterService::group(Count))).await;
  let transport = SseTransport::new(RpcClient::new(format!("http://{addr}")), http());
  assert!(format!("{transport:?}").contains("SseTransport"));
  let client = CounterService::client(transport);
  assert_eq!(run_any(client.twice(21)).await.unwrap(), 42);
  let upto = run_any(client.upto(40).flat_map(|s| s.run_collect()))
    .await
    .unwrap();
  assert_eq!(upto, (1..=40).collect::<Vec<_>>());
  let ticks = run_any(client.ticks(5, None).flat_map(|s| s.run_collect()))
    .await
    .unwrap();
  assert_eq!(ticks.iter().map(|t| t.value).collect::<Vec<_>>(), [5, 6, 7]);
  let resumed = run_any(
    client
      .ticks(5, ticks[0].cursor)
      .flat_map(|s| s.run_collect()),
  )
  .await
  .unwrap();
  assert_eq!(resumed.iter().map(|t| t.value).collect::<Vec<_>>(), [6, 7]);
  // Client streams need the WebSocket transport.
  assert!(run_any(client.sum(vec![1])).await.is_err());
  // Refusals before the stream: unknown procedure (404 Problem), a server that is down.
  let other = listen(server(broadcast::channel(8).0)).await;
  let stranger = CounterService::client(SseTransport::new(
    RpcClient::new(format!("http://{other}")),
    http(),
  ));
  assert!(
    run_any(stranger.upto(1).flat_map(|s| s.run_collect()))
      .await
      .is_err()
  );
  let down = CounterService::client(SseTransport::new(
    RpcClient::new("http://127.0.0.1:1"),
    http(),
  ));
  assert!(
    run_any(down.upto(1).flat_map(|s| s.run_collect()))
      .await
      .is_err()
  );
}
