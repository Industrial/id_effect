//! The call-surface security proof of 0.12 (r12-e12-5-fuzz-security; ADR 0013 applied to the
//! call surface, release spec section "Security").
//!
//! - `every_security_table_test_exists`: every test id the release spec's Security table names
//!   is a test (or a compile-fail case) somewhere in the workspace; a renamed or deleted test
//!   fails here.
//! - `every_0_12_opt_out_is_listed_in_source_and_check_dangerous_covers_it` and
//!   `every_0_12_opt_out_is_listed_and_recorded`: every `dangerous_*` opt-out the 0.12 surface
//!   adds is in `OPT_OUTS`, `scripts/check-dangerous.sh` checks its file and exempts none of
//!   them, and calling each records its id in the posture registry (ADR 0013 R10).
//! - `one_server::cross_crate_cases_against_one_server`: one RPC server over HTTP and
//!   WebSocket, and the defaults that cross crates: the WebSocket `Origin` allowlist
//!   (id_effect_axum realtime), the `x-rpc` header on every POST, the batch cap and the guard
//!   refusal of a batch (`x-rpc-batch-index`), and the per-connection call cap.
//! - `fuzz_targets_cover_the_new_decoders`: the 0.12 decoders have cargo-fuzz targets with
//!   seed corpora (`fuzz/`).

use std::path::{Path, PathBuf};

/// The workspace root.
fn root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
  std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `dir` (recursively), skipping build output.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let path = entry.path();
    let name = entry.file_name();
    if path.is_dir() {
      if name != "target" && name != ".git" {
        rust_files(&path, out);
      }
    } else if path.extension().is_some_and(|e| e == "rs") {
      out.push(path);
    }
  }
}

/// Whether `text` declares `fn <name>` (followed by `(` or `<`).
fn declares_fn(text: &str, name: &str) -> bool {
  let needle = format!("fn {name}");
  text.match_indices(&needle).any(|(at, _)| {
    text[at + needle.len()..]
      .chars()
      .next()
      .is_some_and(|c| c == '(' || c == '<')
  })
}

// ---------------------------------------------------------------------------------------------
// The Security table
// ---------------------------------------------------------------------------------------------

/// Ids of the Security table whose leaf lands after this one (wave 5 in parallel, wave 6); the
/// exit gate (`r12-w8-kpis`) runs with this list empty.
const PENDING: &[(&str, &str)] = &[(
  "posture_prod_refuses_dev_certificate",
  "r12-w6-kit-template",
)];

/// The backticked test ids of the Enforcement-point column of the Security table: snake_case
/// with at least one `_`, not an opt-out name.
fn security_table_ids(spec: &str) -> Vec<String> {
  let section = spec
    .split("\n## ")
    .find(|s| s.starts_with("Security"))
    .expect("the release spec has a Security section");
  let mut ids = Vec::new();
  for row in section.lines().filter(|l| l.starts_with('|')).skip(2) {
    let cells: Vec<&str> = row.split('|').collect();
    let Some(enforcement) = cells.get(4) else {
      continue;
    };
    for (i, part) in enforcement.split('`').enumerate() {
      let id = part.trim();
      let is_id = i % 2 == 1
        && id.contains('_')
        && id
          .bytes()
          .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !id.starts_with("dangerous_")
        && !id.starts_with("insecure_");
      if is_id {
        ids.push(id.to_owned());
      }
    }
  }
  ids
}

#[test]
fn every_security_table_test_exists() {
  let root = root();
  let spec = read(&root.join(".maestro/specs/releases/0.12.0.md"));
  let ids = security_table_ids(&spec);
  assert!(
    ids.len() >= 15,
    "only {} ids in the Security table: {ids:?}",
    ids.len()
  );
  let mut files = Vec::new();
  for dir in ["crates", "tests", "examples"] {
    rust_files(&root.join(dir), &mut files);
  }
  let sources: Vec<(PathBuf, String)> = files
    .into_iter()
    .map(|p| {
      let text = std::fs::read_to_string(&p).unwrap_or_default();
      (p, text)
    })
    .collect();
  let mut missing = Vec::new();
  for id in &ids {
    let compile_fail = root
      .join(format!("tests/security_compile_fail/ui/{id}.rs"))
      .is_file();
    let test_fn = sources.iter().any(|(_, text)| declares_fn(text, id));
    if !(compile_fail || test_fn) && !PENDING.iter().any(|(p, _)| p == id) {
      missing.push(id.clone());
    }
  }
  assert!(
    missing.is_empty(),
    "Security-table tests that do not exist in the workspace: {missing:?}"
  );
  for (id, leaf) in PENDING {
    assert!(
      ids.contains(&(*id).to_owned()),
      "PENDING `{id}` is not in the Security table"
    );
    eprintln!("security_surface: `{id}` is pending on {leaf}");
  }
}

#[test]
fn the_table_parser_reads_the_enforcement_column_only() {
  let spec = "x\n## Security — t\n\n| Threat | Default | Rule | Enforcement point | Leaf |\n\
              |---|---|---|---|---|\n\
              | a | `not_this_one` | R1 | `r1_case`; runtime `some_test`; `dangerous_any_origin` recorded; `Origin` | `leaf_slug` |\n\
              \n## Next\n| x | y | z | `other_test` | w |\n";
  assert_eq!(security_table_ids(spec), ["r1_case", "some_test"]);
  assert!(declares_fn("async fn some_test() {}", "some_test"));
  assert!(declares_fn("fn g<T>() {}", "g"));
  assert!(!declares_fn("fn some_test_longer() {}", "some_test"));
}

// ---------------------------------------------------------------------------------------------
// The 0.12 opt-outs
// ---------------------------------------------------------------------------------------------

/// Every `dangerous_*` opt-out the 0.12 call surface adds: (file under `crates/`, function, the
/// posture id it records).
const OPT_OUTS: &[(&str, &str, &str)] = &[
  (
    "id_effect_axum/src/realtime/origin.rs",
    "dangerous_any_origin",
    "id_effect_axum::realtime::origin::OriginPolicy::dangerous_any_origin",
  ),
  (
    "id_effect_axum/src/realtime/ws.rs",
    "dangerous_any_origin",
    "id_effect_axum::realtime::origin::OriginPolicy::dangerous_any_origin",
  ),
  (
    "id_effect_axum/src/server/connection.rs",
    "dangerous_no_tls_handshake_timeout",
    "id_effect_axum::server::ConnectionLimits::dangerous_no_tls_handshake_timeout",
  ),
  (
    "id_effect_axum/src/server/connection.rs",
    "dangerous_unlimited_concurrent_streams",
    "id_effect_axum::server::ConnectionLimits::dangerous_unlimited_concurrent_streams",
  ),
  (
    "id_effect_axum/src/server/webtransport.rs",
    "dangerous_any_origin",
    "id_effect_axum::realtime::origin::OriginPolicy::dangerous_any_origin",
  ),
  (
    "id_effect_axum/src/server/h3.rs",
    "dangerous_allow_zero_rtt_for_queries",
    "id_effect_axum::server::h3::Http3Options::dangerous_allow_zero_rtt_for_queries",
  ),
  (
    "id_effect_platform/src/quic/config.rs",
    "dangerous_no_retry",
    "id_effect_platform::QuicEndpointConfig::dangerous_no_retry",
  ),
  (
    "id_effect_platform/src/quic/config.rs",
    "dangerous_raise_limits",
    "id_effect_platform::QuicEndpointConfig::dangerous_raise_limits",
  ),
  (
    "id_effect_rpc/src/channels/policy.rs",
    "dangerous_allow_all_joins",
    "id_effect_rpc::channels::dangerous_allow_all_joins",
  ),
  (
    "id_effect_rpc/src/openapi.rs",
    "public_dangerous",
    "id_effect_rpc::openapi::ContractDocs::public_dangerous",
  ),
];

/// The 0.12 sources scanned for opt-outs missing from [`OPT_OUTS`] (directories recursively).
/// `connection.rs` also holds 0.11 opt-outs, so its two 0.12 ones are listed but not scanned.
const SCANNED: &[&str] = &[
  "id_effect_rpc/src",
  "id_effect_platform/src/quic",
  "id_effect_platform/src/tls",
  "id_effect_axum/src/realtime",
  "id_effect_axum/src/server/h3.rs",
  "id_effect_axum/src/server/alt_svc.rs",
  "id_effect_axum/src/server/webtransport.rs",
];

/// `fn` names of the opt-out family (`dangerous_*`, `insecure_*`, `*_dangerous`,
/// `*_insecure`) outside `#[cfg(test)]` modules (a crude cut: everything after the first
/// `#[cfg(test)]` line is a test module in these files).
fn opt_out_fns(text: &str) -> Vec<String> {
  let code = text.split("\n#[cfg(test)]").next().unwrap_or(text);
  let mut names = Vec::new();
  for line in code.lines() {
    let line = line.trim_start();
    if line.starts_with("//") {
      continue;
    }
    let Some(at) = line.find("fn ") else {
      continue;
    };
    if at > 0 && !line[..at].ends_with(' ') {
      continue;
    }
    let name: String = line[at + 3..]
      .chars()
      .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
      .collect();
    if name.starts_with("dangerous_")
      || name.starts_with("insecure_")
      || name.ends_with("_dangerous")
      || name.ends_with("_insecure")
    {
      names.push(name);
    }
  }
  names
}

#[test]
fn every_0_12_opt_out_is_listed_in_source_and_check_dangerous_covers_it() {
  let crates = root().join("crates");
  // Each listed opt-out exists where the list says; recording is checked by calling it
  // (`every_0_12_opt_out_is_listed_and_recorded`) and statically by scripts/check-dangerous.sh,
  // which reads every file under crates/ and exempts only the EXEMPT entries.
  for (file, name, _) in OPT_OUTS {
    let text = read(&crates.join(file));
    assert!(declares_fn(&text, name), "{file} has no fn {name}");
  }
  let script = read(&root().join("scripts/check-dangerous.sh"));
  assert!(
    script.contains("(dangerous|insecure)_") && script.contains("record_dangerous"),
    "scripts/check-dangerous.sh no longer checks the opt-out family"
  );
  let exempt = script
    .split("EXEMPT=(")
    .nth(1)
    .and_then(|rest| rest.split("\n)").next())
    .expect("scripts/check-dangerous.sh has an EXEMPT list");
  for (file, name, _) in OPT_OUTS {
    assert!(
      !exempt.contains(&format!("{file}:{name}")) && !exempt.contains(&format!(":{name}\"")),
      "scripts/check-dangerous.sh exempts the 0.12 opt-out {file}:{name}"
    );
  }
  // Every opt-out in the scanned 0.12 sources is listed.
  let mut files = Vec::new();
  for entry in SCANNED {
    let path = crates.join(entry);
    if path.is_dir() {
      rust_files(&path, &mut files);
    } else if path.is_file() {
      files.push(path);
    }
  }
  let mut unlisted = Vec::new();
  for path in files {
    let rel = path
      .strip_prefix(&crates)
      .unwrap()
      .to_string_lossy()
      .replace('\\', "/");
    for name in opt_out_fns(&read(&path)) {
      if !OPT_OUTS.iter().any(|(f, n, _)| *f == rel && *n == name) {
        unlisted.push(format!("{rel}: {name}"));
      }
    }
  }
  assert!(
    unlisted.is_empty(),
    "0.12 opt-outs missing from OPT_OUTS (list them and check they are recorded): {unlisted:?}"
  );
}

#[test]
fn the_opt_out_scanner_finds_both_forms() {
  let text = "pub fn dangerous_a() {}\n  pub(crate) fn b_insecure(self) {}\n// fn dangerous_c()\n\
              fn safe() {}\n#[cfg(test)]\nmod tests { fn dangerous_test() {} }\n";
  assert_eq!(opt_out_fns(text), ["dangerous_a", "b_insecure"]);
}

#[cfg(all(feature = "ws", feature = "http3", feature = "channels"))]
#[test]
fn every_0_12_opt_out_is_listed_and_recorded() {
  use id_effect::observability::posture::dangerous_opt_outs;
  use id_effect_axum::realtime::origin::OriginPolicy;
  use id_effect_axum::realtime::ws::WsConfig;
  use id_effect_axum::server::ConnectionLimits;
  use id_effect_axum::server::h3::Http3Options;
  use id_effect_platform::quic::QuicEndpointConfig;
  use id_effect_platform::tls::{Identity, NodeCertificate};

  let recorded = |id: &str| dangerous_opt_outs().iter().any(|(i, _)| *i == id);
  let check = |name: &str| {
    let (_, _, id) = OPT_OUTS
      .iter()
      .find(|(_, n, _)| *n == name)
      .unwrap_or_else(|| panic!("{name} is not in OPT_OUTS"));
    assert!(recorded(id), "{name} did not record `{id}`");
  };
  for (_, _, id) in OPT_OUTS {
    assert!(!recorded(id), "`{id}` recorded before any opt-out ran");
  }

  let _ = OriginPolicy::dangerous_any_origin();
  check("dangerous_any_origin");
  let _ = WsConfig::default().dangerous_any_origin();
  #[cfg(feature = "webtransport")]
  let _ =
    id_effect_axum::server::webtransport::WebTransportConfig::default().dangerous_any_origin();
  let _ = ConnectionLimits::default().dangerous_no_tls_handshake_timeout();
  check("dangerous_no_tls_handshake_timeout");
  let _ = ConnectionLimits::default().dangerous_unlimited_concurrent_streams();
  check("dangerous_unlimited_concurrent_streams");
  let options = Http3Options::new();
  assert!(
    !options.admits_early(&axum::http::Method::GET),
    "0-RTT off by default"
  );
  let options = options.dangerous_allow_zero_rtt_for_queries();
  check("dangerous_allow_zero_rtt_for_queries");
  assert!(options.admits_early(&axum::http::Method::GET));
  assert!(
    !options.admits_early(&axum::http::Method::POST),
    "a mutation (POST) is never served from 0-RTT"
  );
  let identity =
    Identity::from_certificate(&NodeCertificate::self_signed("localhost").unwrap()).unwrap();
  let _ = QuicEndpointConfig::new(identity.clone()).dangerous_no_retry();
  check("dangerous_no_retry");
  let _ = QuicEndpointConfig::new(identity).dangerous_raise_limits();
  check("dangerous_raise_limits");
  let _ = id_effect_rpc::channels::dangerous_allow_all_joins();
  check("dangerous_allow_all_joins");
  let _ = id_effect_rpc::openapi::ContractDocs::new(id_effect_rpc::Contract::default())
    .public_dangerous();
  check("public_dangerous");
  for (_, name, id) in OPT_OUTS {
    assert!(recorded(id), "{name}: `{id}` not in the posture registry");
  }
}

// ---------------------------------------------------------------------------------------------
// Fuzz targets
// ---------------------------------------------------------------------------------------------

/// The 0.12 decoder targets (release spec E12.5).
const RPC_FUZZ_TARGETS: &[&str] = &[
  "rpc_wire_request",
  "rpc_batch",
  "rpc_postcard_frame",
  "rpc_duplex_frame",
  "rpc_resume_cursor",
  "sse_last_event_id",
];

#[test]
fn fuzz_targets_cover_the_new_decoders() {
  let fuzz = root().join("fuzz");
  let manifest = read(&fuzz.join("Cargo.toml"));
  let mut targets = 0;
  for entry in std::fs::read_dir(fuzz.join("fuzz_targets"))
    .unwrap()
    .flatten()
  {
    if entry.path().extension().is_some_and(|e| e == "rs") {
      targets += 1;
    }
  }
  assert!(
    targets >= 13,
    "{targets} fuzz targets, 0.12 needs at least 13"
  );
  for target in RPC_FUZZ_TARGETS {
    assert!(
      fuzz.join(format!("fuzz_targets/{target}.rs")).is_file(),
      "no fuzz target {target}"
    );
    assert!(
      manifest.contains(&format!("name = \"{target}\"")),
      "fuzz/Cargo.toml has no [[bin]] {target}"
    );
    let seeds = std::fs::read_dir(fuzz.join(format!("corpus/{target}")))
      .map(|d| {
        d.flatten()
          .filter(|e| e.file_name().to_string_lossy().starts_with("seed-"))
          .count()
      })
      .unwrap_or(0);
    assert!(seeds >= 3, "fuzz/corpus/{target} has {seeds} seeds");
  }
}

// ---------------------------------------------------------------------------------------------
// One server, the cross-crate defaults
// ---------------------------------------------------------------------------------------------

#[cfg(all(feature = "ws", feature = "client"))]
mod one_server {
  use std::net::SocketAddr;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use axum::body::Body;
  use axum::http::request::Parts;
  use axum::http::{Request, StatusCode};
  use http_body_util::BodyExt as _;
  use id_effect::{DynEnv, Never, Stream, schema, succeed};
  use id_effect_axum::realtime::ws::WsConfig;
  use id_effect_axum::route::{GuardFuture, RouteGuard};
  use id_effect_rpc::batch::BATCH_INDEX_HEADER;
  use id_effect_rpc::client::ws::WsClient;
  use id_effect_rpc::kinds::Subscription;
  use id_effect_rpc::transport::ws::RpcWsConfig;
  use id_effect_rpc::wire::{MAX_BATCH_CALLS, RpcFailure};
  use id_effect_rpc::{Procedure, RpcGroup, RpcServer, WireSchema};
  use serde_json::{Value, json};
  use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
  use tower::ServiceExt as _;

  const APP_ORIGIN: &str = "https://app.example";

  /// Admits `authorization: Bearer ok`.
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
            StatusCode::FORBIDDEN,
          ))
        }
      })
    }
  }

  fn text() -> WireSchema<String> {
    WireSchema::new(schema::string::<()>())
  }

  struct Server {
    app: axum::Router,
    addr: SocketAddr,
    runs: Arc<AtomicUsize>,
  }

  async fn server() -> Server {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut group = RpcGroup::new();
    let counter = runs.clone();
    group.mutation(
      Procedure::mutation("Todo.add").public(),
      text(),
      text(),
      move |s: String, _: &mut DynEnv| {
        counter.fetch_add(1, Ordering::SeqCst);
        succeed::<_, Never, _>(s)
      },
    );
    let counter = runs.clone();
    group.mutation(
      Procedure::mutation("Admin.wipe").guard(Bearer),
      text(),
      text(),
      move |s: String, _: &mut DynEnv| {
        counter.fetch_add(1, Ordering::SeqCst);
        succeed::<_, Never, _>(s)
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
    let socket = WsConfig::default()
      .allowed_origins([APP_ORIGIN])
      .unwrap()
      .messages_per_second(10_000);
    let server =
      RpcServer::new(group).with_websocket(RpcWsConfig::default().socket(socket).max_calls(2));
    let app = server.router().with_state(DynEnv::new()).into_axum();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = app.clone();
    tokio::spawn(async move {
      axum::serve(
        listener,
        served.into_make_service_with_connect_info::<SocketAddr>(),
      )
      .await
      .unwrap();
    });
    Server { app, addr, runs }
  }

  async fn post(
    app: &axum::Router,
    body: &Value,
    headers: &[(&str, &str)],
  ) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut req = Request::post("/rpc").header("content-type", "application/json");
    for (name, value) in headers {
      req = req.header(*name, *value);
    }
    let res = app
      .clone()
      .oneshot(req.body(Body::from(body.to_string())).unwrap())
      .await
      .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
      status,
      headers,
      serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
  }

  /// The upgrade's HTTP status with `origin`, or `None` when it succeeded.
  async fn upgrade_status(addr: SocketAddr, origin: Option<&str>) -> Option<StatusCode> {
    let mut req = format!("ws://{addr}/rpc/ws").into_client_request().unwrap();
    req
      .headers_mut()
      .insert("sec-websocket-protocol", "idrpc.json.v1".parse().unwrap());
    if let Some(origin) = origin {
      req.headers_mut().insert("origin", origin.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
      Ok(_) => None,
      Err(tokio_tungstenite::tungstenite::Error::Http(res)) => Some(res.status()),
      Err(err) => panic!("upgrade failed without a status: {err}"),
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn cross_crate_cases_against_one_server() {
    let Server { app, addr, runs } = server().await;
    let rpc = [("x-rpc", "1")];

    // R9 (cross-site WebSocket hijacking): the allowlist of id_effect_axum's realtime layer
    // guards the RPC socket; a look-alike origin is foreign too; a non-browser (no Origin) is
    // served.
    assert_eq!(
      upgrade_status(addr, Some("https://evil.example")).await,
      Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
      upgrade_status(addr, Some("https://app.example.evil")).await,
      Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(upgrade_status(addr, Some(APP_ORIGIN)).await, None);
    assert_eq!(upgrade_status(addr, None).await, None);

    // R9 (CSRF): every POST needs `x-rpc: 1`, single calls and batches alike.
    let call = json!({"tag": "Todo.add", "payload": "milk"});
    for (body, headers) in [
      (&call, &[][..]),
      (&call, &[("x-rpc", "yes")][..]),
      (
        &json!([{"id": 1, "tag": "Todo.add", "payload": "a"}]),
        &[][..],
      ),
    ] {
      let (status, _, problem) = post(&app, body, headers).await;
      assert_eq!(status, StatusCode::FORBIDDEN, "{body} {headers:?}");
      assert_eq!(problem["code"], "RpcHeaderRequired");
    }
    assert_eq!(runs.load(Ordering::SeqCst), 0, "no refused POST ran");
    let (status, _, result) = post(&app, &call, &rpc).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["exit"]["value"], "milk");

    // R4 (batch amplification): over the call cap is a 413 before any call runs.
    let too_many: Vec<Value> = (1..=MAX_BATCH_CALLS as u64 + 1)
      .map(|id| json!({"id": id, "tag": "Todo.add", "payload": "x"}))
      .collect();
    let (status, _, problem) = post(&app, &Value::Array(too_many), &rpc).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(problem["code"], "BatchTooManyCalls");

    // R1 (F28): a guard refusal answers the whole batch, `x-rpc-batch-index` names the refused
    // call, and no call of the batch runs.
    let before = runs.load(Ordering::SeqCst);
    let batch = json!([
      {"id": 1, "tag": "Todo.add", "payload": "a"},
      {"id": 2, "tag": "Admin.wipe", "payload": "all"},
    ]);
    let (status, headers, _) = post(&app, &batch, &rpc).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(headers[BATCH_INDEX_HEADER], "1");
    assert_eq!(
      runs.load(Ordering::SeqCst),
      before,
      "no call of a refused batch ran"
    );

    // R4 (stream exhaustion): the per-connection call cap.
    let url = format!("ws://{addr}/rpc/ws");
    let client = WsClient::connect(&url).await.unwrap();
    let _first = client.open("Feed.idle", json!("a"), None).await.unwrap();
    let _second = client.open("Feed.idle", json!("b"), None).await.unwrap();
    let third = client.call("Todo.add", json!("c")).await.unwrap_err();
    assert_eq!(
      third.reserved(),
      Some(RpcFailure::ResourceExhausted {
        limit: "calls".into()
      })
    );
  }
}
