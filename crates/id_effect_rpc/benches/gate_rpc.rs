//! T12 bench gates of the RPC layer (r12-e12-5-benches, `docs/benchmarks/gates.toml`).
//!
//! In process, through `oneshot` on the router (no network), as `gate/axum_request_*` in
//! `id_effect_axum/benches/gate_request.rs`:
//!
//! - `rpc/unary_json`: a `#[rpc::service]` query taking and returning [`Item`] as JSON,
//!   `POST /rpc/Bench.echo` (`content-type: application/json`, `x-rpc: 1`), result collected.
//! - `rpc/axum_json_handler`: the baseline, a raw axum handler doing the same serde JSON decode
//!   and encode of the same [`Item`] (`Json<Item>` in, `Json<Item>` out). The D5 gate is the
//!   `delta` `rpc/unary_json − rpc/axum_json_handler ≤ 3,000 ns`.
//! - `rpc/unary_postcard`: `rpc/unary_json` with the postcard codec (feature `postcard`).
//!
//! Over loopback TCP, one HTTP/1.1 keep-alive connection, against `axum::serve`:
//!
//! - `rpc/sequential_50`: 50 calls of `Bench.echo`, each awaited before the next.
//! - `rpc/batch_50`: the same 50 calls as one batch (`POST /rpc`, a JSON array; 50 is
//!   `wire::MAX_BATCH_CALLS`). The pair gate is `batch_50 / sequential_50 ≤ 0.20`.
//!
//! Run: `cargo bench -p id_effect_rpc --features postcard --bench gate_rpc`

use std::hint::black_box;
use std::net::SocketAddr;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use criterion::{Criterion, criterion_group, criterion_main};
use http_body_util::{BodyExt, Full};
use id_effect::{DynEnv, Effect, Never, succeed};
use id_effect_rpc as rpc;
use id_effect_rpc::codec::Codec as _;
use id_effect_rpc::codec::postcard::{MEDIA_TYPE, Postcard, decode_result};
use id_effect_rpc::wire::WireExit;
use tower::ServiceExt;

/// A small struct: what a typical query takes and returns.
#[derive(Clone, Debug, PartialEq, id_effect::Schema, serde::Serialize, serde::Deserialize)]
struct Item {
  id: i64,
  name: String,
  done: bool,
}

/// The service under test: one public query.
#[rpc::service(version = 1)]
trait Bench {
  /// The item back.
  #[query]
  #[public]
  fn echo(&self, item: Item) -> Effect<Item, Never, DynEnv>;
}

struct Echo;

impl Bench for Echo {
  fn echo(&self, item: Item) -> Effect<Item, Never, DynEnv> {
    succeed(item)
  }
}

fn item() -> Item {
  Item {
    id: 42,
    name: "bench item".to_owned(),
    done: false,
  }
}

/// The RPC app: `Bench` mounted at the default path `/rpc`.
fn rpc_app() -> axum::Router {
  BenchService::mount(id_effect_axum::route::Router::new(), Echo)
    .with_state(DynEnv::new())
    .into_axum()
}

/// The baseline: plain axum, the same serde decode and encode.
fn axum_app() -> axum::Router {
  axum::Router::new().route(
    "/echo",
    post(|axum::Json(item): axum::Json<Item>| async move { axum::Json(item) }),
  )
}

fn runtime() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime")
}

fn json_post(uri: &str, body: &[u8], rpc_header: bool) -> Request<Body> {
  let mut builder = Request::post(uri).header("content-type", "application/json");
  if rpc_header {
    builder = builder.header("x-rpc", "1");
  }
  builder.body(Body::from(body.to_vec())).expect("request")
}

async fn round_trip(app: &axum::Router, req: Request<Body>) -> Bytes {
  let res = app.clone().oneshot(req).await.expect("infallible router");
  assert_eq!(res.status(), StatusCode::OK);
  res.into_body().collect().await.expect("body").to_bytes()
}

/// The `value` of a JSON `WireResult` that succeeded.
fn success(body: &[u8]) -> serde_json::Value {
  let result: serde_json::Value = serde_json::from_slice(body).expect("a JSON result");
  assert_eq!(result["exit"]["_tag"], "Success", "{result}");
  result["exit"]["value"].clone()
}

fn bench_in_process(c: &mut Criterion) {
  let rt = runtime();
  let mut group = c.benchmark_group("rpc");
  let body = serde_json::to_vec(&item()).expect("json");

  let rpc = rpc_app();
  let answer = rt.block_on(round_trip(&rpc, json_post("/rpc/Bench.echo", &body, true)));
  assert_eq!(success(&answer), serde_json::to_value(item()).unwrap());
  group.bench_function("unary_json", |b| {
    b.iter(|| {
      let req = json_post("/rpc/Bench.echo", &body, true);
      black_box(rt.block_on(round_trip(&rpc, req)).len())
    });
  });

  let raw = axum_app();
  let answer = rt.block_on(round_trip(&raw, json_post("/echo", &body, false)));
  assert_eq!(serde_json::from_slice::<Item>(&answer).unwrap(), item());
  group.bench_function("axum_json_handler", |b| {
    b.iter(|| {
      let req = json_post("/echo", &body, false);
      black_box(rt.block_on(round_trip(&raw, req)).len())
    });
  });

  let postcard = Postcard::encode(&item()).expect("postcard");
  let postcard_post = || {
    Request::post("/rpc/Bench.echo")
      .header("content-type", MEDIA_TYPE)
      .header("x-rpc", "1")
      .body(Body::from(postcard.clone()))
      .expect("request")
  };
  let answer = rt.block_on(round_trip(&rpc, postcard_post()));
  match decode_result(&answer).expect("postcard result").exit {
    WireExit::Success { value } => assert_eq!(Postcard::decode::<Item>(&value).unwrap(), item()),
    WireExit::Failure { error } => panic!("failed: {error}"),
  }
  group.bench_function("unary_postcard", |b| {
    b.iter(|| black_box(rt.block_on(round_trip(&rpc, postcard_post())).len()));
  });

  group.finish();
}

type Sender = hyper::client::conn::http1::SendRequest<Full<Bytes>>;

/// One HTTP/1.1 keep-alive connection to `addr`.
async fn connect(addr: SocketAddr) -> Sender {
  let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
  tcp.set_nodelay(true).expect("nodelay");
  let (send, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
    .await
    .expect("handshake");
  tokio::spawn(conn);
  send
}

async fn tcp_call(send: &mut Sender, uri: &str, body: Bytes) -> Bytes {
  send.ready().await.expect("connection ready");
  let req = Request::post(uri)
    .header("host", "localhost")
    .header("content-type", "application/json")
    .header("x-rpc", "1")
    .body(Full::new(body))
    .expect("request");
  let res = send.send_request(req).await.expect("response");
  assert_eq!(res.status(), StatusCode::OK);
  res.into_body().collect().await.expect("body").to_bytes()
}

fn bench_loopback(c: &mut Criterion) {
  const CALLS: usize = 50;
  let rt = runtime();
  let addr = rt.block_on(async {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = rpc_app().into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await });
    addr
  });
  let mut send = rt.block_on(connect(addr));
  let one = Bytes::from(serde_json::to_vec(&item()).expect("json"));
  let calls: Vec<serde_json::Value> = (1..=CALLS)
    .map(|id| serde_json::json!({"id": id, "tag": "Bench.echo", "payload": item()}))
    .collect();
  let batch = Bytes::from(serde_json::to_vec(&calls).expect("json"));

  // Both shapes answer every call.
  rt.block_on(async {
    let answer = tcp_call(&mut send, "/rpc/Bench.echo", one.clone()).await;
    assert_eq!(success(&answer), serde_json::to_value(item()).unwrap());
    let answer = tcp_call(&mut send, "/rpc", batch.clone()).await;
    let results: Vec<serde_json::Value> = serde_json::from_slice(&answer).expect("batch");
    assert_eq!(results.len(), CALLS);
    assert!(results.iter().all(|r| r["exit"]["_tag"] == "Success"));
  });

  let mut group = c.benchmark_group("rpc");
  group.bench_function("sequential_50", |b| {
    b.iter(|| {
      rt.block_on(async {
        let mut n = 0;
        for _ in 0..CALLS {
          n += tcp_call(&mut send, "/rpc/Bench.echo", one.clone())
            .await
            .len();
        }
        black_box(n)
      })
    });
  });
  group.bench_function("batch_50", |b| {
    b.iter(|| {
      black_box(
        rt.block_on(tcp_call(&mut send, "/rpc", batch.clone()))
          .len(),
      )
    });
  });
  group.finish();
}

criterion_group!(benches, bench_in_process, bench_loopback);
criterion_main!(benches);
