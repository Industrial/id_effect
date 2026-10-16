//! Integration tests of the first-party middleware (`id_effect_axum::middleware`): one test (or
//! more) per layer, driving a real `axum::Router` in process.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderName, Request, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use id_effect::{Effect, ExitKind, HNil, Span, SpanHook, die, sleep, succeed, sync};
use id_effect_axum::PROBLEM_JSON;
use id_effect_axum::middleware::{
  BodyLimitLayer, CompressionLayer, CompressionLevel, DEFAULT_BODY_LIMIT, DEFAULT_TIMEOUT,
  MemoryRateLimitStore, REQUEST_ID_HEADER, REQUEST_SPAN, RateLimitError, RateLimitKey,
  RateLimitLayer, RateLimitStore, RequestId, RequestIdLayer, TimeoutLayer,
};
use serde_json::Value;
use tower::ServiceExt;

async fn send(app: &Router, req: Request<Body>) -> Response {
  app.clone().oneshot(req).await.unwrap()
}

async fn body_bytes(res: Response) -> Bytes {
  res.into_body().collect().await.unwrap().to_bytes()
}

/// Asserts `res` is a Problem with `status` and returns its JSON body.
async fn problem(res: Response, status: StatusCode) -> Value {
  assert_eq!(res.status(), status);
  assert_eq!(res.headers()[header::CONTENT_TYPE], PROBLEM_JSON);
  let body: Value = serde_json::from_slice(&body_bytes(res).await).unwrap();
  assert_eq!(body["status"], status.as_u16());
  body
}

fn get_req(uri: &str) -> Request<Body> {
  Request::builder().uri(uri).body(Body::empty()).unwrap()
}

// ── compression (web.middleware#4) ──────────────────────────────────────────────────────────

const LARGE: usize = 4096;

fn compression_app(layer: CompressionLayer) -> Router {
  Router::new()
    .route("/large", get(|| async { "a".repeat(LARGE) }))
    .route("/small", get(|| async { "tiny" }))
    .layer(layer)
}

fn accepting(uri: &str, encoding: &str) -> Request<Body> {
  Request::builder()
    .uri(uri)
    .header(header::ACCEPT_ENCODING, encoding)
    .body(Body::empty())
    .unwrap()
}

#[tokio::test]
async fn compression_encodes_large_bodies_the_client_accepts() {
  let app = compression_app(CompressionLayer::default());

  let res = send(&app, accepting("/large", "gzip")).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()[header::CONTENT_ENCODING], "gzip");
  let gz = body_bytes(res).await;
  assert_eq!(&gz[..2], &[0x1f, 0x8b], "gzip magic");
  assert!(gz.len() < LARGE / 10);

  let res = send(&app, accepting("/large", "br")).await;
  assert_eq!(res.headers()[header::CONTENT_ENCODING], "br");
  assert!(body_bytes(res).await.len() < LARGE / 10);

  let res = send(&app, accepting("/large", "deflate")).await;
  assert_eq!(res.headers()[header::CONTENT_ENCODING], "deflate");

  // Too small to be worth it, or not accepted: sent as is.
  let res = send(&app, accepting("/small", "gzip")).await;
  assert!(res.headers().get(header::CONTENT_ENCODING).is_none());
  assert_eq!(body_bytes(res).await, "tiny");
  let res = send(&app, get_req("/large")).await;
  assert!(res.headers().get(header::CONTENT_ENCODING).is_none());
  assert_eq!(body_bytes(res).await.len(), LARGE);
}

#[tokio::test]
async fn compression_options_select_encodings_and_threshold() {
  let layer = CompressionLayer::new()
    .gzip(false)
    .deflate(false)
    .br(true)
    .quality(CompressionLevel::Fastest)
    .min_size(0);
  let app = compression_app(layer);
  let res = send(&app, accepting("/large", "gzip")).await;
  assert!(res.headers().get(header::CONTENT_ENCODING).is_none());
  let res = send(&app, accepting("/small", "gzip, br")).await;
  assert_eq!(res.headers()[header::CONTENT_ENCODING], "br");
  assert!(format!("{layer:?}").contains("min_size: 0"));
}

// ── request id + span (web.middleware#5) ────────────────────────────────────────────────────

/// A started span: name and attributes.
type Started = (String, Vec<(String, String)>);

/// Records every span the hook sees: (name, attributes) on start, (name, outcome) on end.
#[derive(Default)]
struct Recorder {
  started: Mutex<Vec<Started>>,
  ended: Mutex<Vec<(String, ExitKind)>>,
}

impl SpanHook for Recorder {
  fn on_start(&self, span: &Span) {
    self
      .started
      .lock()
      .unwrap()
      .push((span.name().to_owned(), span.attributes().to_vec()));
  }

  fn on_end(&self, span: &Span, _end: Instant, exit: ExitKind) {
    self
      .ended
      .lock()
      .unwrap()
      .push((span.name().to_owned(), exit));
  }
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
  attrs
    .iter()
    .find(|(k, _)| k == key)
    .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn request_id_is_propagated_to_handler_response_and_span() {
  let recorder = Arc::new(Recorder::default());
  let app = Router::new()
    .route(
      "/items/{id}",
      get(|id: RequestId| async move { format!("handled {id}") }),
    )
    .layer(RequestIdLayer::new().span_hook(recorder.clone()));

  let req = Request::builder()
    .uri("/items/7")
    .header(REQUEST_ID_HEADER, "abc-123")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, req).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(res.headers()[REQUEST_ID_HEADER], "abc-123");
  assert_eq!(body_bytes(res).await, "handled abc-123");

  let started = recorder.started.lock().unwrap().clone();
  assert_eq!(started.len(), 1);
  let (name, attrs) = &started[0];
  assert_eq!(name, REQUEST_SPAN);
  assert_eq!(attr(attrs, "http.request.method"), Some("GET"));
  assert_eq!(attr(attrs, "http.route"), Some("/items/{id}"));
  assert_eq!(attr(attrs, "request_id"), Some("abc-123"));
  assert_eq!(
    recorder.ended.lock().unwrap().as_slice(),
    &[(REQUEST_SPAN.to_owned(), ExitKind::Success)]
  );

  // An invalid incoming id is replaced by a generated one.
  let req = Request::builder()
    .uri("/items/8")
    .header(REQUEST_ID_HEADER, "not valid!")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, req).await;
  let generated = res.headers()[REQUEST_ID_HEADER]
    .to_str()
    .unwrap()
    .to_owned();
  assert_ne!(generated, "not valid!");
  assert!(RequestId::parse(&generated).is_some());
  assert_eq!(body_bytes(res).await, format!("handled {generated}"));
}

#[tokio::test]
async fn request_id_header_and_trust_are_configurable() {
  let header = HeaderName::from_static("x-correlation-id");
  let app = Router::new()
    .route(
      "/",
      get(|id: Option<RequestId>| async move { id.map(|id| id.to_string()).unwrap_or_default() }),
    )
    .layer(
      RequestIdLayer::default()
        .header(header.clone())
        .trust_incoming(false),
    );
  let req = Request::builder()
    .uri("/unrouted")
    .header(&header, "client-chosen")
    .body(Body::empty())
    .unwrap();
  let res = send(&app, req).await;
  assert_eq!(res.status(), StatusCode::NOT_FOUND);
  assert_ne!(res.headers()[&header], "client-chosen");

  let res = send(&app, get_req("/")).await;
  let id = res.headers()[&header].to_str().unwrap().to_owned();
  assert_eq!(body_bytes(res).await, id);
}

#[tokio::test]
async fn request_id_extractor_without_the_layer_is_a_500_problem() {
  let app = Router::new()
    .route("/", get(|id: RequestId| async move { id.to_string() }))
    .route(
      "/optional",
      get(|id: Option<RequestId>| async move { format!("{}", id.is_some()) }),
    );
  problem(
    send(&app, get_req("/")).await,
    StatusCode::INTERNAL_SERVER_ERROR,
  )
  .await;
  assert_eq!(
    body_bytes(send(&app, get_req("/optional")).await).await,
    "false"
  );
}

#[tokio::test]
async fn request_span_records_status_in_the_tracing_buffer() {
  id_effect::install_tracing_layer(id_effect::TracingConfig::enabled());
  let app = Router::new()
    .route("/teapot", get(|| async { StatusCode::IM_A_TEAPOT }))
    .layer(RequestIdLayer::new());
  let res = send(&app, get_req("/teapot")).await;
  assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
  let snap = id_effect::snapshot_tracing();
  let span = snap
    .spans
    .iter()
    .rev()
    .find(|s| s.name == REQUEST_SPAN)
    .expect("request span recorded");
  assert_eq!(
    span
      .annotations
      .get("http.response.status_code")
      .map(String::as_str),
    Some("418")
  );
}

// ── timeout (web.middleware#8) ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn timeout_answers_503_problem_and_interrupts_the_handler_fiber() {
  let finalized = Arc::new(AtomicBool::new(false));
  let flag = finalized.clone();
  let app = Router::new()
    .route(
      "/slow",
      id_effect_axum::routing::get(move |_: &mut ()| {
        let flag = flag.clone();
        sleep::<Infallible, ()>(Duration::from_secs(30))
          .map(|()| "done")
          .ensuring(sync(move || flag.store(true, Ordering::SeqCst)))
      }),
    )
    .route("/fast", get(|| async { "fast" }))
    .layer(TimeoutLayer::new(Duration::from_millis(50)));

  let started = Instant::now();
  let res = send(&app, get_req("/slow")).await;
  assert!(started.elapsed() < Duration::from_secs(10));
  assert_eq!(res.headers()[header::RETRY_AFTER], "1");
  let body = problem(res, StatusCode::SERVICE_UNAVAILABLE).await;
  assert_eq!(body["instance"], "/slow");
  assert!(body["detail"].as_str().unwrap().contains("50 ms"));

  tokio::time::timeout(Duration::from_secs(10), async {
    while !finalized.load(Ordering::SeqCst) {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("the interrupted handler fiber ran its finalizer");

  let res = send(&app, get_req("/fast")).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(TimeoutLayer::default().timeout(), DEFAULT_TIMEOUT);
}

// ── body limit (web.middleware#8) ───────────────────────────────────────────────────────────

fn echo_app(limit: BodyLimitLayer, calls: Arc<AtomicUsize>) -> Router {
  Router::new()
    .route(
      "/echo",
      post(move |body: Bytes| {
        calls.fetch_add(1, Ordering::SeqCst);
        async move { body.len().to_string() }
      }),
    )
    .layer(limit)
}

fn post_req(body: Body, content_length: Option<usize>) -> Request<Body> {
  let mut req = Request::builder().method("POST").uri("/echo");
  if let Some(len) = content_length {
    req = req.header(header::CONTENT_LENGTH, len);
  }
  req.body(body).unwrap()
}

#[tokio::test]
async fn body_limit_answers_413_problem_for_declared_and_streamed_bodies() {
  let calls = Arc::new(AtomicUsize::new(0));
  let app = echo_app(BodyLimitLayer::new(16), calls.clone());

  // Declared too large: refused before the handler runs.
  let res = send(&app, post_req(Body::from(vec![b'x'; 100]), Some(100))).await;
  let body = problem(res, StatusCode::PAYLOAD_TOO_LARGE).await;
  assert!(body["detail"].as_str().unwrap().contains("16 bytes"));
  assert_eq!(calls.load(Ordering::SeqCst), 0);

  // Streamed without Content-Length: cut off while reading.
  let chunks = (0..10).map(|_| Bytes::from_static(b"0123456789")).collect();
  let res = send(&app, post_req(Body::new(Chunked(chunks)), None)).await;
  problem(res, StatusCode::PAYLOAD_TOO_LARGE).await;

  // Within the limit.
  let res = send(&app, post_req(Body::from("small"), Some(5))).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_bytes(res).await, "5");
}

#[tokio::test]
async fn body_limit_above_axums_default_is_honoured() {
  let calls = Arc::new(AtomicUsize::new(0));
  let big = DEFAULT_BODY_LIMIT + 1024;
  let app = echo_app(BodyLimitLayer::new(4 * 1024 * 1024), calls.clone());
  let res = send(&app, post_req(Body::from(vec![b'x'; big]), None)).await;
  assert_eq!(res.status(), StatusCode::OK);
  assert_eq!(body_bytes(res).await, big.to_string());
  assert_eq!(BodyLimitLayer::default().limit(), DEFAULT_BODY_LIMIT);
}

/// A chunked body of unknown length (no `Content-Length`, no exact size hint).
struct Chunked(std::collections::VecDeque<Bytes>);

impl http_body::Body for Chunked {
  type Data = Bytes;
  type Error = Infallible;

  fn poll_frame(
    mut self: std::pin::Pin<&mut Self>,
    _: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
    std::task::Poll::Ready(self.0.pop_front().map(|b| Ok(http_body::Frame::data(b))))
  }
}

// ── rate limit (web.middleware#6) ───────────────────────────────────────────────────────────

fn from_ip(ip: [u8; 4]) -> Request<Body> {
  let mut req = get_req("/api");
  req
    .extensions_mut()
    .insert(ConnectInfo(SocketAddr::from((ip, 40_000))));
  req
}

#[tokio::test]
async fn rate_limit_answers_429_per_client_ip() {
  let app = Router::new()
    .route("/api", get(|| async { "ok" }))
    .layer(RateLimitLayer::per_second(0.0, 2.0));

  for _ in 0..2 {
    assert_eq!(
      send(&app, from_ip([10, 0, 0, 1])).await.status(),
      StatusCode::OK
    );
  }
  let res = send(&app, from_ip([10, 0, 0, 1])).await;
  assert_eq!(res.headers()[header::RETRY_AFTER], "60");
  let body = problem(res, StatusCode::TOO_MANY_REQUESTS).await;
  assert_eq!(body["instance"], "/api");

  // Another client has its own bucket.
  assert_eq!(
    send(&app, from_ip([10, 0, 0, 2])).await.status(),
    StatusCode::OK
  );
}

/// A store that counts calls and fails (or dies) as configured.
struct ScriptedStore {
  calls: AtomicUsize,
  die: bool,
}

impl RateLimitStore for ScriptedStore {
  fn acquire(&self, _key: &str) -> Effect<(), RateLimitError, HNil> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    if self.die {
      die("store unreachable")
    } else {
      succeed(())
    }
  }
}

#[tokio::test]
async fn rate_limit_store_and_key_are_pluggable() {
  let store = Arc::new(ScriptedStore {
    calls: AtomicUsize::new(0),
    die: true,
  });
  let exempt_health =
    RateLimitKey::from_fn(|req| (req.uri().path() != "/health").then(|| "tenant:acme".to_owned()));
  let layer = RateLimitLayer::new(store.clone())
    .key(exempt_health)
    .retry_after(Duration::from_secs(5));
  assert!(format!("{layer:?}").contains("5s"));
  let app = Router::new()
    .route("/api", get(|| async { "ok" }))
    .route("/health", get(|| async { "up" }))
    .layer(layer);

  // A store that dies fails open.
  assert_eq!(send(&app, get_req("/api")).await.status(), StatusCode::OK);
  assert_eq!(store.calls.load(Ordering::SeqCst), 1);
  // A request without a key is not limited (and the store is not asked).
  assert_eq!(
    send(&app, get_req("/health")).await.status(),
    StatusCode::OK
  );
  assert_eq!(store.calls.load(Ordering::SeqCst), 1);

  // Shared in-memory buckets keyed by API key.
  let shared = Arc::new(MemoryRateLimitStore::new(0.0, 1.0));
  let app =
    Router::new()
      .route("/api", get(|| async { "ok" }))
      .layer(RateLimitLayer::new(shared.clone()).key(
        RateLimitKey::header(HeaderName::from_static("x-api-key")).or(RateLimitKey::global()),
      ));
  let keyed = |key: &str| {
    Request::builder()
      .uri("/api")
      .header("x-api-key", key)
      .body(Body::empty())
      .unwrap()
  };
  assert_eq!(send(&app, keyed("k1")).await.status(), StatusCode::OK);
  assert_eq!(
    send(&app, keyed("k1")).await.status(),
    StatusCode::TOO_MANY_REQUESTS
  );
  assert_eq!(send(&app, keyed("k2")).await.status(), StatusCode::OK);
  assert_eq!(shared.len(), 2);
}

// ── the recommended stack ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn layers_compose_and_rejections_carry_the_request_id() {
  let app = Router::new()
    .route("/", post(|body: String| async move { body }))
    .layer(CompressionLayer::new())
    .layer(TimeoutLayer::new(Duration::from_secs(5)))
    .layer(BodyLimitLayer::new(8))
    .layer(RateLimitLayer::per_second(100.0, 100.0))
    .layer(RequestIdLayer::new());
  let req = Request::builder()
    .method("POST")
    .uri("/")
    .header(REQUEST_ID_HEADER, "req-1")
    .header(header::CONTENT_LENGTH, 20)
    .body(Body::from(vec![b'x'; 20]))
    .unwrap();
  let res = send(&app, req).await;
  assert_eq!(res.headers()[REQUEST_ID_HEADER], "req-1");
  problem(res, StatusCode::PAYLOAD_TOO_LARGE).await;
}
