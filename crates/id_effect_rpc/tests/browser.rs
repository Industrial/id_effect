//! D12 browser test (r12-e12-3-webtransport): Chromium driven from Rust over CDP
//! (`headless_chrome`, chosen over `chromiumoxide` because its tungstenite is the tree's one)
//! opens a page whose client prefers WebTransport, then WebSocket, and runs a two-way call:
//!
//! 1. with WebTransport served (HTTP/3, a 13-day ECDSA P-256 certificate pinned by
//!    `serverCertificateHashes`), the call runs over WebTransport;
//! 2. with nothing listening on the WebTransport port, the same page falls back to WebSocket.
//!
//! Runs only with `ID_EFFECT_REQUIRE_BROWSER=1` (then a missing Chromium or `openssl` fails the
//! test, it never skips); without it the test returns at once. The certificate is made with the
//! `openssl` CLI here; `id-effect dev` (r12-w6-kit-template) makes the same kind for apps.
#![cfg(feature = "webtransport")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use headless_chrome::{Browser, LaunchOptions};
use id_effect::{DynEnv, Never, schema, succeed};
use id_effect_axum::realtime::ws::WsConfig;
use id_effect_axum::server::ConnectionLimits;
use id_effect_axum::server::h3::{Http3Options, serve_h3};
use id_effect_axum::server::webtransport::{self as wt, WebTransportConfig};
use id_effect_platform::quic::{QuicEndpoint, QuicEndpointConfig};
use id_effect_platform::tls::{Identity, NodeCertificate};
use id_effect_rpc::transport::webtransport::{self, RpcWebTransportConfig};
use id_effect_rpc::transport::ws::RpcWsConfig;
use id_effect_rpc::{Duplex, Procedure, RpcGroup, RpcServer, WireSchema};
use serde_json::Value;

const REQUIRE: &str = "ID_EFFECT_REQUIRE_BROWSER";

/// The page's origins (plain HTTP on loopback is a secure context).
const ORIGINS: [&str; 1] = ["http://127.0.0.1:*"];

const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>id_effect rpc transports</title>
<script>
const enc = new TextEncoder(), dec = new TextDecoder();
function record(frame) {
  const bytes = enc.encode(JSON.stringify(frame));
  const out = new Uint8Array(4 + bytes.length);
  new DataView(out.buffer).setUint32(0, bytes.length);
  out.set(bytes, 4);
  return out;
}
function frames(readable) {
  const reader = readable.getReader();
  let buf = new Uint8Array(0);
  return async function next() {
    for (;;) {
      if (buf.length >= 4) {
        const n = new DataView(buf.buffer, buf.byteOffset).getUint32(0);
        if (buf.length >= 4 + n) {
          const frame = JSON.parse(dec.decode(buf.slice(4, 4 + n)));
          buf = buf.slice(4 + n);
          return frame;
        }
      }
      const { value, done } = await reader.read();
      if (done) return null;
      const grown = new Uint8Array(buf.length + value.length);
      grown.set(buf);
      grown.set(value, buf.length);
      buf = grown;
    }
  };
}
const call = { id: 1, kind: "call", tag: "Echo.double", payload: null };
const items = [1, 2, 3].map((n) => ({ id: 1, kind: "item", flags: n === 3 ? 1 : 0, payload: n }));
async function viaWebTransport(url, hashHex) {
  if (typeof WebTransport === "undefined") throw new Error("no WebTransport");
  const hash = new Uint8Array(hashHex.match(/../g).map((h) => parseInt(h, 16)));
  const wt = new WebTransport(url, { serverCertificateHashes: [{ algorithm: "sha-256", value: hash }] });
  await Promise.race([
    wt.ready,
    new Promise((_, reject) => setTimeout(() => reject(new Error("webtransport timeout")), 8000)),
  ]);
  const control = await wt.createBidirectionalStream();
  await control.writable.getWriter().write(record({ id: 0, kind: "ping" }));
  const hello = await frames(control.readable)();
  if (!hello || hello.kind !== "hello") throw new Error("no hello");
  const stream = await wt.createBidirectionalStream();
  const writer = stream.writable.getWriter();
  const next = frames(stream.readable);
  await writer.write(record(call));
  for (const item of items) await writer.write(record(item));
  const got = [];
  for (;;) {
    const frame = await next();
    if (!frame || frame.kind === "exit") break;
    if (frame.kind === "item") got.push(frame.payload);
  }
  wt.close();
  return { transport: "webtransport", items: got };
}
function viaWebSocket(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url, "idrpc.json.v1");
    const got = [];
    ws.onmessage = (event) => {
      const frame = JSON.parse(event.data);
      if (frame.kind === "hello") {
        ws.send(JSON.stringify(call));
        for (const item of items) ws.send(JSON.stringify(item));
      } else if (frame.kind === "item") {
        got.push(frame.payload);
      } else if (frame.kind === "exit") {
        ws.close();
        resolve({ transport: "websocket", items: got });
      }
    };
    ws.onerror = () => reject(new Error("websocket error"));
  });
}
(async () => {
  const params = new URLSearchParams(location.search);
  let result;
  try {
    result = await viaWebTransport(params.get("wt"), params.get("hash"));
  } catch (error) {
    try {
      result = await viaWebSocket(params.get("ws"));
      result.why = String(error);
    } catch (again) {
      result = { transport: "none", why: String(error) + " / " + String(again) };
    }
  }
  window.__result = result;
})();
</script>
"#;

fn int() -> WireSchema<i64> {
  WireSchema::new(schema::i64::<()>())
}

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.bidi(
    Procedure::bidi("Echo.double").public(),
    int(),
    int(),
    |_: &mut DynEnv| succeed::<_, Never, _>(Duplex::new(|items| items.map(|n| n * 2))),
  );
  group
}

/// A fresh directory under the system temp dir.
fn scratch() -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "id-effect-browser-{}-{}",
    std::process::id(),
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos()
  ));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

/// A 13-day ECDSA P-256 certificate for 127.0.0.1 (what `serverCertificateHashes` accepts:
/// at most 14 days, ECDSA).
fn dev_certificate() -> NodeCertificate {
  let dir = scratch();
  let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
  let status = std::process::Command::new("openssl")
    .args([
      "req",
      "-x509",
      "-newkey",
      "ec",
      "-pkeyopt",
      "ec_paramgen_curve:prime256v1",
    ])
    .args(["-nodes", "-days", "13", "-subj", "/CN=localhost"])
    .args(["-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1"])
    .arg("-keyout")
    .arg(&key)
    .arg("-out")
    .arg(&cert)
    .stderr(std::process::Stdio::null())
    .status()
    .expect("openssl on PATH (ID_EFFECT_REQUIRE_BROWSER=1)");
  assert!(status.success(), "openssl failed: {status}");
  NodeCertificate::from_pem_files(&cert, &key).unwrap()
}

async fn serve_tcp(app: axum::Router) -> SocketAddr {
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

/// The RPC server: WebSocket on TCP, WebTransport on QUIC. Returns (tcp, udp, certificate hash).
async fn serve_rpc() -> (SocketAddr, SocketAddr, String) {
  let socket = WsConfig::default().allowed_origins(ORIGINS).unwrap();
  let server = RpcServer::new(group()).with_websocket(RpcWsConfig::default().socket(socket));
  let session = WebTransportConfig::default()
    .allowed_origins(ORIGINS)
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
  let tcp = serve_tcp(app.clone()).await;
  let cert = dev_certificate();
  let hash: String = cert
    .cert_sha256()
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect();
  let identity = Identity::from_certificate(&cert).unwrap();
  let limits = ConnectionLimits::default();
  let quic = QuicEndpoint::bind(
    "127.0.0.1:0".parse().unwrap(),
    QuicEndpointConfig::new(identity).with_limits(wt::quic_limits(&limits)),
  )
  .unwrap();
  let udp = quic.local_addr();
  tokio::spawn(async move {
    let options = Http3Options::default().webtransport();
    serve_h3(&quic, app, limits, options, std::future::pending()).await
  });
  (tcp, udp, hash)
}

/// A UDP port with nothing behind it.
fn dead_udp_port() -> u16 {
  let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
  socket.local_addr().unwrap().port()
}

/// Opens each URL in headless Chromium and returns what its page reported.
fn run_pages(urls: Vec<String>) -> Vec<Value> {
  let options = LaunchOptions::default_builder()
    .headless(true)
    .sandbox(false)
    .idle_browser_timeout(Duration::from_secs(120))
    .build()
    .unwrap();
  let browser = Browser::new(options).expect("Chromium on PATH (ID_EFFECT_REQUIRE_BROWSER=1)");
  let tab = browser.new_tab().unwrap();
  urls
    .into_iter()
    .map(|url| {
      tab.navigate_to(&url).unwrap();
      tab.wait_until_navigated().unwrap();
      let deadline = Instant::now() + Duration::from_secs(60);
      loop {
        let reported = tab
          .evaluate("JSON.stringify(window.__result || null)", false)
          .unwrap()
          .value
          .and_then(|v| v.as_str().map(str::to_owned))
          .and_then(|s| serde_json::from_str::<Value>(&s).ok())
          .filter(|v| !v.is_null());
        if let Some(result) = reported {
          return result;
        }
        assert!(
          Instant::now() < deadline,
          "the page reported nothing: {url}"
        );
        std::thread::sleep(Duration::from_millis(100));
      }
    })
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browser_prefers_webtransport_then_websocket() {
  if std::env::var(REQUIRE).as_deref() != Ok("1") {
    eprintln!("browser test not run: set {REQUIRE}=1 (Chromium and openssl on PATH)");
    return;
  }
  let (tcp, udp, hash) = serve_rpc().await;
  let page = axum::Router::new().route(
    "/",
    axum::routing::get(|| async { axum::response::Html(PAGE) }),
  );
  let origin = serve_tcp(page).await;
  let ws = format!("ws://127.0.0.1:{}/rpc/ws", tcp.port());
  let url = |wt: String| format!("http://{origin}/?wt={wt}&hash={hash}&ws={ws}");
  let served = url(format!("https://127.0.0.1:{}/rpc/wt", udp.port()));
  let dead = url(format!("https://127.0.0.1:{}/rpc/wt", dead_udp_port()));
  let results = tokio::task::spawn_blocking(move || run_pages(vec![served, dead]))
    .await
    .unwrap();
  let doubled = serde_json::json!([2, 4, 6]);
  assert_eq!(results[0]["transport"], "webtransport", "{}", results[0]);
  assert_eq!(results[0]["items"], doubled);
  assert_eq!(results[1]["transport"], "websocket", "{}", results[1]);
  assert_eq!(results[1]["items"], doubled);
}
