//! Every call opens one `rpc.request` span, whatever the transport (web.rpc#8,
//! r12-f-rpc-spans): over HTTP per request; over WebSocket and WebTransport per call, under the
//! connection's `rpc.connection` span; over SSE per stream, under the `GET`'s HTTP span. A
//! capturing subscriber (installed globally: the server runs on other threads) records every
//! span; each test uses its own tags.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, Once};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use id_effect::{DynEnv, Never, schema, succeed};
use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
use tower::ServiceExt as _;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

// --- the capturing subscriber ------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Captured {
  name: &'static str,
  fields: HashMap<String, String>,
  parent: Option<u64>,
}

impl Captured {
  fn field(&self, name: &str) -> &str {
    self.fields.get(name).map_or("", String::as_str)
  }
}

#[derive(Default)]
struct Spans {
  next: AtomicU64,
  spans: Mutex<HashMap<u64, Captured>>,
}

static SPANS: LazyLock<Spans> = LazyLock::new(Spans::default);

thread_local! {
  static ENTERED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

struct Fields<'a>(&'a mut HashMap<String, String>);

impl Visit for Fields<'_> {
  fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
    self.0.insert(field.name().to_owned(), format!("{value:?}"));
  }

  fn record_str(&mut self, field: &Field, value: &str) {
    self.0.insert(field.name().to_owned(), value.to_owned());
  }
}

struct Capture;

impl Subscriber for Capture {
  fn enabled(&self, _: &Metadata<'_>) -> bool {
    true
  }

  fn new_span(&self, attrs: &Attributes<'_>) -> Id {
    let id = SPANS.next.fetch_add(1, Ordering::Relaxed) + 1;
    let parent = if attrs.is_root() {
      None
    } else if let Some(parent) = attrs.parent() {
      Some(parent.into_u64())
    } else {
      ENTERED.with(|entered| entered.borrow().last().copied())
    };
    let mut span = Captured {
      name: attrs.metadata().name(),
      fields: HashMap::new(),
      parent,
    };
    attrs.record(&mut Fields(&mut span.fields));
    SPANS.spans.lock().unwrap().insert(id, span);
    Id::from_u64(id)
  }

  fn record(&self, span: &Id, values: &Record<'_>) {
    if let Some(span) = SPANS.spans.lock().unwrap().get_mut(&span.into_u64()) {
      values.record(&mut Fields(&mut span.fields));
    }
  }

  fn record_follows_from(&self, _: &Id, _: &Id) {}

  fn event(&self, _: &Event<'_>) {}

  fn enter(&self, span: &Id) {
    ENTERED.with(|entered| entered.borrow_mut().push(span.into_u64()));
  }

  fn exit(&self, _: &Id) {
    ENTERED.with(|entered| entered.borrow_mut().pop());
  }
}

fn capture() {
  static INSTALL: Once = Once::new();
  INSTALL.call_once(|| tracing::subscriber::set_global_default(Capture).unwrap());
}

/// The `rpc.request` spans of `tag` on `transport`.
fn requests(tag: &str, transport: &str) -> Vec<Captured> {
  SPANS
    .spans
    .lock()
    .unwrap()
    .values()
    .filter(|s| s.name == "rpc.request")
    .filter(|s| s.field("rpc.operation") == tag && s.field("rpc.transport") == transport)
    .cloned()
    .collect()
}

fn span(id: u64) -> Captured {
  SPANS.spans.lock().unwrap()[&id].clone()
}

/// The common fields of an `rpc.request` span of `Spans.<method>`.
fn assert_call_fields(span: &Captured, method: &str) {
  assert_eq!(span.field("rpc.system"), "id_effect_rpc", "{span:?}");
  assert_eq!(span.field("rpc.service"), "Spans", "{span:?}");
  assert_eq!(span.field("rpc.method"), method, "{span:?}");
  assert!(!span.field("correlation.id").is_empty(), "{span:?}");
}

// --- the server --------------------------------------------------------------------------------

fn text() -> WireSchema<String> {
  WireSchema::new(schema::string::<()>())
}

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  for tag in ["Spans.http", "Spans.ws", "Spans.wt"] {
    group.query(
      Procedure::query(tag).public(),
      text(),
      text(),
      |name: String, _: &mut DynEnv| succeed::<_, Never, _>(format!("Hello, {name}!")),
    );
  }
  #[cfg(feature = "sse")]
  group.server_stream(
    Procedure::server_stream("Spans.sse").public(),
    text(),
    text(),
    |name: String, _: &mut DynEnv| {
      succeed::<_, Never, _>(id_effect::Stream::from_iterable([name.clone(), name]))
    },
  );
  group
}

fn app(server: RpcServer) -> axum::Router {
  server.router().with_state(DynEnv::new()).into_axum()
}

// --- the transports ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_call_emits_rpc_request_span() {
  capture();
  let app = app(RpcServer::new(group()));
  for _ in 0..2 {
    let request = Request::post("/rpc/Spans.http")
      .header("content-type", "application/json")
      .header("x-rpc", "1")
      .body(Body::from("\"Ada\""))
      .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("Hello, Ada!"), "{body}");
  }
  let spans = requests("Spans.http", "http");
  assert_eq!(spans.len(), 2, "one span per call: {spans:?}");
  for span in &spans {
    assert_call_fields(span, "http");
    assert_eq!(span.field("http.method"), "POST");
    assert_eq!(span.field("http.route"), "/rpc");
  }
  assert_ne!(
    spans[0].field("correlation.id"),
    spans[1].field("correlation.id")
  );
}

#[cfg(all(feature = "ws", feature = "client"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_call_emits_rpc_request_span() {
  use id_effect_rpc::client::ws::WsClient;
  use id_effect_rpc::transport::ws::RpcWsConfig;
  use serde_json::json;

  capture();
  let app = app(RpcServer::new(group()).with_websocket(RpcWsConfig::default()));
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move {
    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, service).await.unwrap();
  });
  let client = WsClient::connect(&format!("ws://{addr}/rpc/ws"))
    .await
    .unwrap();
  for _ in 0..2 {
    let answer = client.call("Spans.ws", json!("Ada")).await.unwrap();
    assert_eq!(answer, json!("Hello, Ada!"));
  }
  let spans = requests("Spans.ws", "websocket");
  assert_eq!(spans.len(), 2, "one span per call: {spans:?}");
  for call in &spans {
    assert_call_fields(call, "ws");
  }
  // Both calls are children of the one connection's span.
  assert_eq!(spans[0].parent, spans[1].parent);
  let connection = span(spans[0].parent.expect("a parent"));
  assert_eq!(connection.name, "rpc.connection");
  assert_eq!(connection.field("rpc.transport"), "websocket");
  assert_eq!(connection.field("rpc.system"), "id_effect_rpc");
  // A call's correlation id is the connection's, numbered by the call.
  let conn_id = connection.field("correlation.id");
  assert!(
    spans.iter().all(|s| s
      .field("correlation.id")
      .starts_with(&format!("{conn_id}-"))),
    "{spans:?}"
  );
}

#[cfg(feature = "sse")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_call_emits_rpc_request_span() {
  capture();
  let app = app(RpcServer::new(group()));
  let request = Request::get("/rpc/Spans.sse?input=%22a%22")
    .header("accept", "text/event-stream")
    .body(Body::empty())
    .unwrap();
  let response = app.oneshot(request).await.unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let body = response.into_body().collect().await.unwrap().to_bytes();
  let body = String::from_utf8_lossy(&body);
  assert!(body.contains("event: exit"), "{body}");
  let spans = requests("Spans.sse", "sse");
  assert_eq!(spans.len(), 1, "one span per stream: {spans:?}");
  assert_call_fields(&spans[0], "sse");
  // Under the GET's HTTP span, with its correlation id.
  let get = span(spans[0].parent.expect("a parent"));
  assert_eq!(get.name, "rpc.request");
  assert_eq!(get.field("rpc.transport"), "http");
  assert_eq!(get.field("rpc.operation"), "Spans.sse");
  assert_eq!(
    get.field("correlation.id"),
    spans[0].field("correlation.id")
  );
}

#[cfg(feature = "webtransport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn webtransport_call_emits_rpc_request_span() {
  use std::time::Duration;

  use id_effect_axum::server::ConnectionLimits;
  use id_effect_axum::server::h3::{Http3Options, serve_h3};
  use id_effect_axum::server::webtransport::client::{self, ConnectRequest};
  use id_effect_axum::server::webtransport::{self as wt, WebTransportConfig};
  use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};
  use id_effect_platform::tls::{Identity, NodeCertificate, client_tls_config};
  use id_effect_rpc::codec::postcard::{encode_record, split_record};
  use id_effect_rpc::transport::webtransport::{self, RpcWebTransportConfig};
  use serde_json::{Value, json};
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  const APP: &str = "https://app.example";

  /// The next JSON record of `stream`.
  async fn next(stream: &mut wt::WebTransportStream, buf: &mut Vec<u8>) -> Value {
    loop {
      if let Some((record, used)) = split_record(buf).unwrap() {
        let value = serde_json::from_slice(record).unwrap();
        buf.drain(..used);
        return value;
      }
      let mut chunk = [0_u8; 4096];
      let read = stream.recv.read(&mut chunk);
      let n = tokio::time::timeout(Duration::from_secs(20), read)
        .await
        .unwrap()
        .unwrap();
      assert!(n > 0, "the stream ended");
      buf.extend_from_slice(&chunk[..n]);
    }
  }

  async fn send(stream: &mut wt::WebTransportStream, frame: Value) {
    let record = encode_record(frame.to_string().as_bytes()).unwrap();
    stream.send.write_all(&record).await.unwrap();
  }

  capture();
  let server = RpcServer::new(group());
  let session = WebTransportConfig::default()
    .allowed_origins([APP])
    .unwrap();
  let route = webtransport::endpoint(
    server.clone(),
    RpcWebTransportConfig::default().session(session),
  );
  let app = server
    .router()
    .route(route)
    .with_state(DynEnv::new())
    .into_axum();
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
  let (_stop, stopped) = tokio::sync::oneshot::channel::<()>();
  tokio::spawn(async move {
    let options = Http3Options::default().webtransport();
    serve_h3(&quic, app, limits, options, async {
      let _ = stopped.await;
    })
    .await
  });
  let tls = client_tls_config(&identity, None, &[pin]).unwrap();
  let request = ConnectRequest {
    origin: Some(APP.to_owned()),
    headers: Vec::new(),
  };
  let session = client::connect(addr, "localhost", tls, "/rpc/wt", &request)
    .await
    .unwrap();
  let mut control = session.open_bi().await.unwrap();
  let mut control_buf = Vec::new();
  send(&mut control, json!({"id": 0, "kind": "ping"})).await;
  assert_eq!(next(&mut control, &mut control_buf).await["kind"], "hello");
  for id in 1..=2 {
    let mut call = session.open_bi().await.unwrap();
    let mut buf = Vec::new();
    let frame = json!({"id": id, "kind": "call", "tag": "Spans.wt", "payload": "Ada"});
    send(&mut call, frame).await;
    let exit = loop {
      let frame = next(&mut call, &mut buf).await;
      if frame["kind"] == "exit" {
        break frame;
      }
    };
    assert_eq!(exit["payload"]["value"], "Hello, Ada!", "{exit}");
  }
  let spans = requests("Spans.wt", "webtransport");
  assert_eq!(spans.len(), 2, "one span per call: {spans:?}");
  for call in &spans {
    assert_call_fields(call, "wt");
  }
  assert_eq!(spans[0].parent, spans[1].parent);
  let connection = span(spans[0].parent.expect("a parent"));
  assert_eq!(connection.name, "rpc.connection");
  assert_eq!(connection.field("rpc.transport"), "webtransport");
}
