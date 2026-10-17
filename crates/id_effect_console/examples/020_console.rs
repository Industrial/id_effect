//! Serves the console (dashboard page and JSON API) behind an operator guard on a loopback port,
//! fetches the page and every API route over the socket with and without a credential, changes
//! the log level, prints what came back and exits.
//!
//! `cargo run -p id_effect_console --example 020_console`
//!
//! `cargo run -p id_effect_console --example 020_console -- --serve` keeps serving instead and
//! treats every request as the local operator, so a browser can open the printed URL. That is
//! only acceptable because the listener is bound to loopback (`ConsoleConfig::requires_guard`).

use std::net::SocketAddr;

use axum::Router;
use axum::extract::Request;
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::Response;
use id_effect::{HNil, Never, succeed};
use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, require_role};
use id_effect_axum::{HealthIndicator, HealthOutcome, HealthRegistry};
use id_effect_console::{Console, ConsoleConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The test credential the example's authentication layer accepts.
const TOKEN: &str = "example-operator-token";

fn operator() -> CurrentPrincipal {
  CurrentPrincipal::new(BasicPrincipal::new("ops").with_role("operator"))
}

/// A bearer-token authentication layer (an app uses `id_effect_auth` here).
async fn authenticate(mut request: Request, next: Next) -> Response {
  let bearer = request
    .headers()
    .get(header::AUTHORIZATION)
    .and_then(|v| v.to_str().ok())
    .and_then(|v| v.strip_prefix("Bearer "));
  if bearer == Some(TOKEN) {
    request.extensions_mut().insert(operator());
  }
  next.run(request).await
}

/// `--serve`: every request is the local operator.
async fn local_operator(mut request: Request, next: Next) -> Response {
  request.extensions_mut().insert(operator());
  next.run(request).await
}

/// One HTTP/1.1 request; the status line and the body.
async fn send(
  addr: SocketAddr,
  method: &str,
  path: &str,
  token: Option<&str>,
  body: &str,
) -> (String, String) {
  let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
  if let Some(token) = token {
    request.push_str(&format!("Authorization: Bearer {token}\r\n"));
  }
  if !body.is_empty() {
    request.push_str("Content-Type: application/json\r\n");
  }
  request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
  let mut stream = TcpStream::connect(addr).await.expect("connect");
  stream.write_all(request.as_bytes()).await.expect("write");
  let mut raw = Vec::new();
  stream.read_to_end(&mut raw).await.expect("read");
  let raw = String::from_utf8_lossy(&raw).into_owned();
  let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
  let status = head.lines().next().unwrap_or_default();
  let status = status.split_once(' ').map_or(status, |(_, s)| s).to_owned();
  (status, body.to_owned())
}

fn summary(body: &str) -> String {
  let body = body.trim();
  if body.chars().count() <= 96 {
    body.to_owned()
  } else {
    let head: String = body.chars().take(96).collect();
    format!("{head}… ({} bytes)", body.len())
  }
}

#[tokio::main]
async fn main() {
  let serve_forever = std::env::args().any(|a| a == "--serve");
  let config = ConsoleConfig::loopback(if serve_forever { 9090 } else { 0 });

  let health = HealthRegistry::new().with_indicator(HealthIndicator::new("db", || {
    succeed::<_, Never, HNil>(HealthOutcome::up())
  }));
  let console = Console::new()
    .health(health)
    .router(Guard::new("console.operate", require_role("operator")));
  let app: Router = if serve_forever {
    console.layer(middleware::from_fn(local_operator))
  } else {
    console.layer(middleware::from_fn(authenticate))
  };

  let listener = TcpListener::bind(config.bind()).await.expect("bind");
  let addr = listener.local_addr().expect("local address");
  if serve_forever {
    println!("console on http://{addr}/_console (Ctrl-C to stop)");
    axum::serve(listener, app).await.expect("serve");
    return;
  }
  tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });

  let (status, _) = send(addr, "GET", "/_console", None, "").await;
  println!("GET /_console without a credential: {status}");
  assert!(status.starts_with("401"), "the page is guarded");
  let (status, page) = send(addr, "GET", "/_console", Some(TOKEN), "").await;
  println!(
    "GET /_console as operator:          {status} ({} bytes of HTML)",
    page.len()
  );
  assert!(status.starts_with("200") && page.contains("<title>id_effect console</title>"));

  for route in ["fibers", "supervisors", "metrics", "health", "log-level"] {
    let path = format!("/_console/api/{route}");
    let (denied, _) = send(addr, "GET", &path, None, "").await;
    assert!(denied.starts_with("401"), "{path} is guarded");
    let (status, body) = send(addr, "GET", &path, Some(TOKEN), "").await;
    println!("GET {path:<28} {status}  {}", summary(&body));
    assert!(status.starts_with("200"), "{path}: {status}");
  }
  let (status, body) = send(
    addr,
    "PUT",
    "/_console/api/log-level",
    Some(TOKEN),
    r#"{"level":"debug"}"#,
  )
  .await;
  println!(
    "PUT /_console/api/log-level      {status}  {}",
    summary(&body)
  );
  assert!(status.starts_with("200"), "PUT log-level: {status}");
}
