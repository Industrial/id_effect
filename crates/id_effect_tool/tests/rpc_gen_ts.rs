//! `id-effect rpc gen ts` (r12-e12-4-gen-ts): the generated TypeScript is byte-for-byte the
//! golden output (`tests/golden/rpc/**`, `UPDATE_GOLDEN=1` rewrites it), type-checks with
//! `deno check`, and its tests (`tests/ts/*.test.ts`) pass under `deno test` against a real
//! server on loopback: every kind, batching per microtask, reconnect and resume.
//!
//! Deno comes from devenv (`deno` on `PATH`; CI pins it with `denoland/setup-deno`). Without it
//! the Deno tests print `SKIP`, unless `ID_EFFECT_REQUIRE_DENO=1`, which turns a missing Deno into
//! a failure (CI sets it).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use id_effect::{DynEnv, Effect, Never, Stream, fail, from_async, succeed};
use id_effect_axum::realtime::sse::SseConfig;
use id_effect_axum::realtime::ws::WsConfig;
use id_effect_axum::route::GuardFuture;
use id_effect_rpc as rpc;
use rpc::contract::{Contract, assert_snapshot};
use rpc::kinds::{Duplex, Subscription, SubscriptionItem};
use rpc::registry::RouteGuard;
use rpc::service::{RpcSchema, RpcService};
use rpc::transport::ws::RpcWsConfig;
use rpc::wire::{Cursor, InputIssue};
use rpc::{RpcGroup, RpcServer, WireSchema};
use serde_json::json;

// ---------------------------------------------------------------------------------------------
// The test service
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Price {
  cents: i64,
  currency: String,
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Product {
  id: i64,
  name: String,
  price: Price,
  note: Option<String>,
  tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "_tag")]
enum Status {
  Open,
  Shipped { tracking: String },
}

/// An order: a branded id, an ISO 8601 time, a map of lines and a tagged union (a hand-written
/// schema, so the contract carries every shape the generator maps).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Order {
  id: String,
  placed_at: String,
  lines: BTreeMap<String, i64>,
  status: Status,
  #[serde(default)]
  note: Option<String>,
}

impl RpcSchema for Order {
  fn wire_schema() -> WireSchema<Self> {
    WireSchema::from_fns(
      |value| {
        serde_json::from_value(value.clone()).map_err(|e| {
          vec![InputIssue {
            path: String::new(),
            message: e.to_string(),
          }]
        })
      },
      |order| serde_json::to_value(order).map_err(|e| e.to_string()),
      json!({
        "type": "object",
        "properties": {
          "id": {"$ref": "#/$defs/rpc_gen_ts::OrderId"},
          "placed_at": {"$ref": "#/$defs/rpc_gen_ts::Timestamp"},
          "lines": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 1}},
          "status": {"$ref": "#/$defs/rpc_gen_ts::Status"},
          "note": {"anyOf": [{"type": "string"}, {"type": "null"}]}
        },
        "required": ["id", "placed_at", "lines", "status"],
        "$defs": {
          "rpc_gen_ts::OrderId": {"type": "string", "pattern": "^o-[0-9]+$", "description": "An order id."},
          "rpc_gen_ts::Timestamp": {"type": "string", "format": "date-time"},
          "rpc_gen_ts::Status": {"oneOf": [
            {"type": "object", "properties": {"_tag": {"const": "Open"}}, "required": ["_tag"]},
            {"type": "object", "properties": {"_tag": {"const": "Shipped"}, "tracking": {"type": "string"}}, "required": ["_tag", "tracking"]}
          ]}
        }
      }),
    )
  }
}

#[derive(Debug, PartialEq, rpc::RpcErrorUnion)]
enum ShopError {
  #[rpc(status = 404)]
  NotFound { id: i64 },
  #[rpc(status = 404)]
  UnknownOrder { id: String },
  #[rpc(status = 409)]
  SoldOut,
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

/// A small shop: the TypeScript client's test service.
#[rpc::service(version = 1)]
trait Shop {
  /// One product by id.
  #[query]
  #[public]
  fn product(&self, id: i64) -> Effect<Product, ShopError, DynEnv>;

  /// One order by id (`o-1` exists).
  #[query]
  #[public]
  fn order(&self, id: String) -> Effect<Order, ShopError, DynEnv>;

  /// Places an order; anyone may, and an `Idempotency-Key` replays it.
  #[mutation]
  #[public]
  fn place(&self, order: Order) -> Effect<Order, ShopError, DynEnv>;

  /// Restocks a product (staff only).
  #[mutation]
  #[guard(Staff)]
  fn restock(&self, id: i64) -> Effect<i64, ShopError, DynEnv>;

  /// Answers `ms` after `ms` milliseconds.
  #[query]
  #[public]
  fn sleep(&self, ms: i64) -> Effect<i64, Never, DynEnv>;

  /// `n`, `n - 1`, …, `1`; a negative `n` fails after one item.
  #[server_stream]
  #[public]
  fn countdown(&self, n: i64) -> Effect<Stream<i64, ShopError, DynEnv>, ShopError, DynEnv>;

  /// The sum of the client's numbers.
  #[client_stream(max_items = 1000)]
  #[public]
  fn total(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv>;

  /// Every line back in upper case.
  #[bidi]
  #[public]
  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv>;

  /// Five ticks from `from`, resumable by cursor.
  #[subscription]
  #[public]
  fn ticks(
    &self,
    from: i64,
    after: Option<Cursor>,
  ) -> Effect<Subscription<i64, Never>, Never, DynEnv>;
}

/// The shop's second version, served beside the first.
#[rpc::service(version = 2, name = "Shop")]
trait ShopV2 {
  /// Every product name.
  #[query]
  #[public]
  fn names(&self) -> Effect<Vec<String>, Never, DynEnv>;
}

#[derive(Default)]
struct Store {
  placed: Arc<AtomicI64>,
}

fn sample_order(id: &str) -> Order {
  Order {
    id: id.to_owned(),
    placed_at: "2026-10-19T08:00:00Z".to_owned(),
    lines: BTreeMap::from([("tea".to_owned(), 2)]),
    status: Status::Shipped {
      tracking: "T-1".to_owned(),
    },
    note: None,
  }
}

impl Shop for Store {
  fn product(&self, id: i64) -> Effect<Product, ShopError, DynEnv> {
    match id {
      1 => succeed(Product {
        id,
        name: "Tea".into(),
        price: Price {
          cents: 350,
          currency: "EUR".into(),
        },
        note: None,
        tags: vec!["hot".into()],
      }),
      2 => fail(ShopError::SoldOut),
      3 => fail(ShopError::Corrupt {
        detail: "secret".into(),
      }),
      _ => fail(ShopError::NotFound { id }),
    }
  }

  fn order(&self, id: String) -> Effect<Order, ShopError, DynEnv> {
    if id == "o-1" {
      succeed(sample_order(&id))
    } else {
      fail(ShopError::UnknownOrder { id })
    }
  }

  fn place(&self, order: Order) -> Effect<Order, ShopError, DynEnv> {
    let n = self.placed.fetch_add(1, Ordering::SeqCst) + 1;
    succeed(Order {
      id: format!("o-{}", 100 + n),
      status: Status::Open,
      ..order
    })
  }

  fn restock(&self, id: i64) -> Effect<i64, ShopError, DynEnv> {
    succeed(id)
  }

  fn sleep(&self, ms: i64) -> Effect<i64, Never, DynEnv> {
    from_async(move || async move {
      tokio::time::sleep(Duration::from_millis(ms.clamp(0, 10_000) as u64)).await;
      Ok(ms)
    })
  }

  fn countdown(&self, n: i64) -> Effect<Stream<i64, ShopError, DynEnv>, ShopError, DynEnv> {
    if n < 0 {
      succeed(Stream::from_vec(vec![1]).chain(Stream::fail(ShopError::NotFound { id: n })))
    } else {
      succeed(Stream::from_iterable((1..=n).rev()))
    }
  }

  fn total(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv> {
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
    let items = (start..from + 5)
      .map(|n| SubscriptionItem::new(n).with_cursor(Cursor::events(n as u64)))
      .collect();
    succeed(Subscription::new(Stream::from_vec(items)))
  }
}

impl ShopV2 for Store {
  fn names(&self) -> Effect<Vec<String>, Never, DynEnv> {
    succeed(vec!["Tea".to_owned(), "Coffee".to_owned()])
  }
}

fn shop_contract() -> Contract {
  ShopService::document().with_service(ShopV2Service::contract())
}

/// The shop on loopback (HTTP, WebSocket, SSE), on a runtime the caller keeps alive.
fn serve() -> (tokio::runtime::Runtime, SocketAddr) {
  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_all()
    .build()
    .unwrap();
  let addr = rt.block_on(async {
    let mut group = RpcGroup::new();
    ShopService::register(&mut group, Store::default());
    ShopV2Service::register(&mut group, Store::default());
    let socket = WsConfig::default().messages_per_second(100_000);
    let server = RpcServer::new(group)
      .with_websocket(RpcWsConfig::default().socket(socket))
      .with_sse(SseConfig::default());
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
  });
  (rt, addr)
}

// ---------------------------------------------------------------------------------------------
// Golden files
// ---------------------------------------------------------------------------------------------

fn manifest() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn golden() -> PathBuf {
  manifest().join("tests/golden/rpc")
}

fn bless() -> bool {
  ["UPDATE_GOLDEN", "ID_EFFECT_BLESS"]
    .iter()
    .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
}

/// The generated files of `contract` equal the ones in `dir` (rewritten when blessing).
fn assert_generated(contract: &Contract, dir: &Path) {
  let files = id_effect_tool::rpc::gen_ts::generate(contract).unwrap();
  assert_eq!(
    files.iter().map(|f| f.path).collect::<Vec<_>>(),
    id_effect_tool::rpc::gen_ts::FILES
  );
  for file in files {
    let path = dir.join(file.path);
    if bless() {
      std::fs::create_dir_all(dir).unwrap();
      std::fs::write(&path, &file.contents).unwrap();
      continue;
    }
    let want = std::fs::read_to_string(&path)
      .unwrap_or_else(|e| panic!("{}: {e} (UPDATE_GOLDEN=1 writes it)", path.display()));
    assert!(
      want == file.contents,
      "{} differs from the generator's output (UPDATE_GOLDEN=1 rewrites it)",
      path.display()
    );
  }
}

#[test]
fn gen_ts_golden_output() {
  // The test service: its contract snapshot, then its client.
  let shop = golden().join("shop");
  assert_snapshot(&shop_contract(), shop.join("rpc.contract.json"));
  assert_generated(&shop_contract(), &shop);
  // Every schema shape, name collisions and keywords, from a hand-written contract.
  let types = golden().join("types");
  let text = std::fs::read_to_string(types.join("rpc.contract.json")).unwrap();
  let contract = Contract::from_json(&text).unwrap();
  assert_generated(&contract, &types);
  // Deterministic: the same output for the services in another order, and on every run.
  let mut shuffled = contract.clone();
  shuffled.services.reverse();
  let once = id_effect_tool::rpc::gen_ts::generate(&shuffled).unwrap();
  assert_eq!(
    once,
    id_effect_tool::rpc::gen_ts::generate(&contract).unwrap()
  );
  assert_eq!(
    once,
    id_effect_tool::rpc::gen_ts::generate(&shuffled).unwrap()
  );
}

fn cli(args: &[&str]) -> (ExitCode, String, String) {
  let (mut out, mut err) = (Vec::new(), Vec::new());
  let code = id_effect_tool::cli::run(
    std::iter::once("id-effect").chain(args.iter().copied()),
    &mut out,
    &mut err,
  );
  (
    code,
    String::from_utf8(out).unwrap(),
    String::from_utf8(err).unwrap(),
  )
}

#[test]
fn rpc_gen_ts_command_writes_the_golden_files() {
  let tmp = tempfile::tempdir().unwrap();
  let out = tmp.path().join("web/rpc");
  let contract = golden().join("shop/rpc.contract.json");
  let (code, stdout, stderr) = cli(&[
    "rpc",
    "gen",
    "ts",
    "--contract",
    contract.to_str().unwrap(),
    "--out",
    out.to_str().unwrap(),
  ]);
  assert_eq!(code, ExitCode::SUCCESS, "{stderr}");
  for name in id_effect_tool::rpc::gen_ts::FILES {
    assert!(
      stdout.contains(&format!("wrote {}", out.join(name).display())),
      "{stdout}"
    );
    assert_eq!(
      std::fs::read_to_string(out.join(name)).unwrap(),
      std::fs::read_to_string(golden().join("shop").join(name)).unwrap(),
      "{name}"
    );
  }
}

#[test]
fn rpc_gen_ts_command_refusals_exit_1() {
  let tmp = tempfile::tempdir().unwrap();
  let dir = tmp.path();
  let out = dir.join("out");
  let out = out.to_str().unwrap();
  let run = |contract: &Path| {
    cli(&[
      "rpc",
      "gen",
      "ts",
      "--contract",
      contract.to_str().unwrap(),
      "--out",
      out,
    ])
  };
  let (code, _, err) = run(&dir.join("missing.json"));
  assert_eq!(code, ExitCode::FAILURE);
  assert!(err.starts_with("error: cannot read "), "{err}");
  std::fs::write(
    dir.join("bad.json"),
    r#"{"format":"other/1","services":[]}"#,
  )
  .unwrap();
  let (code, _, err) = run(&dir.join("bad.json"));
  assert_eq!(code, ExitCode::FAILURE);
  assert!(
    err.contains("unsupported contract format `other/1`"),
    "{err}"
  );
  // The output directory is a file.
  std::fs::write(dir.join("out"), "").unwrap();
  let (code, _, err) = run(&golden().join("shop/rpc.contract.json"));
  assert_eq!(code, ExitCode::FAILURE);
  assert!(err.starts_with("error: cannot write "), "{err}");
  let (code, _, err) = cli(&["rpc", "gen"]);
  assert_eq!(code, ExitCode::from(2), "{err}");
  let (code, out, _) = cli(&["rpc", "gen", "ts", "--help"]);
  assert_eq!(code, ExitCode::SUCCESS);
  assert!(
    out.contains("--contract <FILE>") && out.contains("--out <DIR>"),
    "{out}"
  );
}

#[test]
fn generator_errors_have_sources() {
  use id_effect_tool::rpc::gen_ts::{GenTsError, generate_to};
  use std::error::Error as _;
  let err = generate_to(
    Path::new("/nonexistent/rpc.contract.json"),
    Path::new("/tmp"),
  )
  .unwrap_err();
  assert!(matches!(err, GenTsError::Read { .. }) && err.source().is_some());
  let tmp = tempfile::tempdir().unwrap();
  let file = tmp.path().join("c.json");
  std::fs::write(&file, "[]").unwrap();
  let err = generate_to(&file, tmp.path()).unwrap_err();
  assert!(
    matches!(err, GenTsError::Contract(_)) && err.source().is_some(),
    "{err}"
  );
}

// ---------------------------------------------------------------------------------------------
// Deno
// ---------------------------------------------------------------------------------------------

/// `deno` on `PATH`, or `None` (a failure under `ID_EFFECT_REQUIRE_DENO=1`).
fn deno() -> Option<PathBuf> {
  let found = std::env::var_os("PATH").and_then(|paths| {
    std::env::split_paths(&paths)
      .map(|dir| dir.join("deno"))
      .find(|p| p.is_file())
  });
  let required = std::env::var("ID_EFFECT_REQUIRE_DENO").is_ok_and(|v| v == "1");
  match found {
    Some(path) => Some(path),
    None if required => panic!("ID_EFFECT_REQUIRE_DENO=1 but `deno` is not on PATH"),
    None => {
      eprintln!(
        "SKIP: `deno` is not on PATH (devenv provides it; ID_EFFECT_REQUIRE_DENO=1 fails instead)"
      );
      None
    }
  }
}

/// Run `deno <args>` in the crate directory with a cache of its own, for at most two minutes;
/// panics with its output on failure.
fn run_deno(deno: &Path, args: &[&str]) {
  let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("deno");
  let logs = tempfile::tempdir().unwrap();
  let (out, err) = (logs.path().join("out"), logs.path().join("err"));
  let mut child = Command::new(deno)
    .args(args)
    .current_dir(manifest())
    .env("DENO_DIR", cache)
    .env("NO_COLOR", "1")
    .env("DENO_NO_UPDATE_CHECK", "1")
    .stdout(std::fs::File::create(&out).unwrap())
    .stderr(std::fs::File::create(&err).unwrap())
    .spawn()
    .expect("deno runs");
  let started = std::time::Instant::now();
  let status = loop {
    if let Some(status) = child.try_wait().unwrap() {
      break Some(status);
    }
    if started.elapsed() > Duration::from_secs(120) {
      let _ = child.kill();
      let _ = child.wait();
      break None;
    }
    std::thread::sleep(Duration::from_millis(50));
  };
  assert!(
    status.is_some_and(|s| s.success()),
    "deno {} ({})\n--- stdout\n{}\n--- stderr\n{}",
    args.join(" "),
    status.map_or_else(|| "killed after 120 s".to_owned(), |s| s.to_string()),
    String::from_utf8_lossy(&std::fs::read(&out).unwrap()),
    String::from_utf8_lossy(&std::fs::read(&err).unwrap())
  );
}

/// `deno test` of one test file against the shop.
fn deno_test(file: &str) {
  let Some(deno) = deno() else { return };
  let (_rt, addr) = serve();
  let url = format!("http://{addr}/rpc");
  let net = format!("--allow-net={addr}");
  run_deno(
    &deno,
    &[
      "test",
      "--no-config",
      "--no-remote",
      "--no-prompt",
      &net,
      file,
      "--",
      &url,
    ],
  );
}

/// Serves the shop for `deno test … -- <url>` by hand: `cargo test -p id_effect_tool --test
/// rpc_gen_ts serve_the_shop -- --ignored --nocapture` prints the URL and serves for ten minutes.
#[test]
#[ignore = "serves the test service for manual Deno runs"]
fn serve_the_shop() {
  let (_rt, addr) = serve();
  println!("http://{addr}/rpc");
  std::thread::sleep(Duration::from_secs(600));
}

#[test]
fn generated_client_type_checks() {
  let Some(deno) = deno() else { return };
  let mut files: Vec<String> = Vec::new();
  for dir in [
    "tests/golden/rpc/shop",
    "tests/golden/rpc/types",
    "tests/ts",
  ] {
    let mut names: Vec<String> = std::fs::read_dir(manifest().join(dir))
      .unwrap()
      .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
      .filter(|n| n.ends_with(".ts"))
      .map(|n| format!("{dir}/{n}"))
      .collect();
    names.sort();
    files.extend(names);
  }
  let mut args = vec!["check", "--no-config", "--no-remote"];
  args.extend(files.iter().map(String::as_str));
  run_deno(&deno, &args);
}

#[test]
fn generated_client_validates_frames() {
  deno_test("tests/ts/frames.test.ts");
}

#[test]
fn generated_client_round_trip_against_test_server() {
  deno_test("tests/ts/round_trip.test.ts");
}

#[test]
fn generated_client_batches_same_microtask_calls() {
  deno_test("tests/ts/batching.test.ts");
}

#[test]
fn generated_client_resumes_after_reconnect() {
  deno_test("tests/ts/resume.test.ts");
}

/// The shop on the host entry point (F43: `ServerHandle` with `server.http3.webtransport` on):
/// HTTPS and HTTP/3 on one loopback port, WebTransport at `/rpc/wt`, with the 13-day ECDSA
/// P-256 development certificate of `id-effect dev` issued into `dir`.
fn serve_webtransport(
  dir: &Path,
) -> (
  tokio::runtime::Runtime,
  id_effect_axum::ServerHandle,
  id_effect_tool::dev::cert::DevCertificate,
) {
  let cert = id_effect_tool::dev::cert::ensure(dir, std::time::SystemTime::now()).unwrap();
  let rt = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_all()
    .build()
    .unwrap();
  let handle = rt.block_on(async {
    let mut group = RpcGroup::new();
    ShopService::register(&mut group, Store::default());
    ShopV2Service::register(&mut group, Store::default());
    let server = RpcServer::new(group);
    let route = rpc::transport::webtransport::endpoint(
      server.clone(),
      rpc::transport::webtransport::RpcWebTransportConfig::default(),
    );
    let app = server
      .router()
      .route(route)
      .with_state(DynEnv::new())
      .into_axum();
    let config = id_effect_axum::HostConfig {
      bind_host: "127.0.0.1".into(),
      bind_port: 0,
      ..id_effect_axum::HostConfig::default()
    }
    .with_tls(id_effect_axum::server::HostTls::files(
      &cert.cert, &cert.key,
    ))
    .with_webtransport();
    id_effect_axum::ServerHandle::start(&config, app)
      .await
      .unwrap()
  });
  (rt, handle, cert)
}

/// F45: the generated client's streams over WebTransport under the pinned Deno (`deno` from
/// devenv, 2.7.x; CI's `denoland/setup-deno` pin), whose `WebTransport` needs `--unstable-net`.
/// The server is the host entry point with WebTransport on; the client pins the certificate with
/// `serverCertificateHashes`, and `--cert` makes Deno's `fetch` trust the development CA for
/// the HTTPS queries.
#[test]
fn generated_client_webtransport_round_trip_deno() {
  let Some(deno) = deno() else { return };
  let dir = tempfile::tempdir().unwrap();
  let (_rt, handle, cert) = serve_webtransport(dir.path());
  let addr = handle.local_addr();
  let url = format!("https://127.0.0.1:{}/rpc", addr.port());
  let session = format!("{url}/wt");
  let net = format!("--allow-net=127.0.0.1:{}", addr.port());
  let ca = format!("--cert={}", cert.ca.display());
  let hash = cert.sha256_hex();
  run_deno(
    &deno,
    &[
      "test",
      "--no-config",
      "--no-remote",
      "--no-prompt",
      "--unstable-net",
      &ca,
      &net,
      "tests/ts/webtransport.test.ts",
      "--",
      &url,
      &session,
      &hash,
    ],
  );
}
