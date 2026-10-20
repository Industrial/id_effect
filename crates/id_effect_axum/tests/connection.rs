//! Integration tests of the server's accept loop (`id_effect_axum::server::connection`) over
//! real sockets: the peer address on every request (H1), the header-read timeout, the
//! connection cap and the keep-alive limit (M1), and graceful shutdown.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ConnectInfo;
use axum::routing::get;
use id_effect_axum::middleware::{MemoryRateLimitStore, RateLimitKey, RateLimitLayer};
use id_effect_axum::{
  ClientIp, ConnectionLimits, HostConfig, ReadinessState, ServerHandle, TrustedProxies,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};

const WAIT: Duration = Duration::from_secs(5);

async fn start(app: Router, limits: ConnectionLimits) -> ServerHandle {
  let config = HostConfig {
    bind_host: "127.0.0.1".into(),
    bind_port: 0,
    shutdown_timeout: Duration::from_secs(5),
    ..HostConfig::default()
  };
  ServerHandle::start_with_limits(&config, app, ReadinessState::new(true), limits)
    .await
    .unwrap()
}

/// The peer's address as the handler sees it.
fn whoami() -> Router {
  Router::new()
    .route(
      "/ip",
      get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
    )
    .route(
      "/slow",
      get(|| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "done"
      }),
    )
}

/// A connection to `server` from the local address `from`.
async fn connect_from(from: Ipv4Addr, server: SocketAddr) -> TcpStream {
  let socket = TcpSocket::new_v4().unwrap();
  socket.bind(SocketAddr::from((from, 0))).unwrap();
  socket.connect(server).await.unwrap()
}

fn get_request(path: &str, extra: &str) -> String {
  format!("GET {path} HTTP/1.1\r\nHost: test\r\n{extra}\r\n")
}

/// One request on a fresh connection from `from`, closed after the response.
async fn exchange(from: Ipv4Addr, server: SocketAddr, path: &str, extra: &str) -> String {
  let mut stream = connect_from(from, server).await;
  let request = get_request(path, &format!("{extra}Connection: close\r\n"));
  stream.write_all(request.as_bytes()).await.unwrap();
  let mut out = String::new();
  tokio::time::timeout(WAIT, stream.read_to_string(&mut out))
    .await
    .expect("a response in time")
    .unwrap();
  out
}

/// Reads one response (its head and a `Content-Length` body) without waiting for the close.
async fn read_response(stream: &mut TcpStream) -> String {
  let mut buf = Vec::new();
  let mut chunk = [0_u8; 1024];
  loop {
    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
      let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
      let len = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
      if buf.len() >= end + 4 + len {
        return String::from_utf8_lossy(&buf[..end + 4 + len]).into_owned();
      }
    }
    let n = tokio::time::timeout(WAIT, stream.read(&mut chunk))
      .await
      .expect("a response in time")
      .unwrap();
    assert!(
      n > 0,
      "closed mid-response: {}",
      String::from_utf8_lossy(&buf)
    );
    buf.extend_from_slice(&chunk[..n]);
  }
}

/// Whether the server closes `stream` within `within` (EOF or reset), reading whatever it sends.
async fn closed_within(stream: &mut TcpStream, within: Duration) -> Option<String> {
  let mut out = Vec::new();
  match tokio::time::timeout(within, stream.read_to_end(&mut out)).await {
    Ok(_) => Some(String::from_utf8_lossy(&out).into_owned()),
    Err(_elapsed) => None,
  }
}

const LOCAL: Ipv4Addr = Ipv4Addr::LOCALHOST;

#[tokio::test]
async fn h1_per_ip_limit_sees_the_peer() {
  // One request per client, never refilled.
  let app = whoami().layer(RateLimitLayer::per_second(0.0, 1.0));
  let server = start(app, ConnectionLimits::default()).await;
  let addr = server.local_addr();

  let first = exchange(LOCAL, addr, "/ip", "").await;
  assert!(first.starts_with("HTTP/1.1 200"), "{first}");
  assert!(
    first.ends_with("127.0.0.1"),
    "the handler sees the peer: {first}"
  );
  let second = exchange(LOCAL, addr, "/ip", "").await;
  assert!(second.starts_with("HTTP/1.1 429"), "{second}");
  // Another client has a bucket of its own (Linux routes all of 127/8 to loopback).
  if cfg!(target_os = "linux") {
    let other = exchange(Ipv4Addr::new(127, 0, 0, 2), addr, "/ip", "").await;
    assert!(other.starts_with("HTTP/1.1 200"), "{other}");
    assert!(other.ends_with("127.0.0.2"), "{other}");
  }
  server.shutdown().await;
}

#[tokio::test]
async fn h1_forwarded_for_behind_a_trusted_proxy_keys_the_client() {
  // The test plays the proxy at 127.0.0.1, appending the client it saw.
  let trusted = TrustedProxies::new(["127.0.0.1/32"]).unwrap();
  let app = Router::new()
    .route(
      "/ip",
      get(|ClientIp(ip): ClientIp| async move { ip.to_string() }),
    )
    .layer(
      RateLimitLayer::new(MemoryRateLimitStore::new(0.0, 1.0))
        .key(RateLimitKey::client_ip(trusted.clone())),
    )
    .layer(axum::Extension(trusted));
  let server = start(app, ConnectionLimits::default()).await;
  let addr = server.local_addr();
  let from = |chain: &str| format!("X-Forwarded-For: {chain}\r\n");

  // The client prepends a made-up hop; the proxy's entry (rightmost) is what counts.
  let first = exchange(LOCAL, addr, "/ip", &from("6.6.6.6, 198.51.100.1")).await;
  assert!(first.starts_with("HTTP/1.1 200"), "{first}");
  assert!(first.ends_with("198.51.100.1"), "{first}");
  for spoof in ["7.7.7.7, 198.51.100.1", "198.51.100.1"] {
    let again = exchange(LOCAL, addr, "/ip", &from(spoof)).await;
    assert!(again.starts_with("HTTP/1.1 429"), "{spoof}: {again}");
  }
  let other = exchange(LOCAL, addr, "/ip", &from("203.0.113.5")).await;
  assert!(other.starts_with("HTTP/1.1 200"), "{other}");
  server.shutdown().await;
}

#[tokio::test]
async fn m1_slow_headers_are_cut_off() {
  let limits = ConnectionLimits::new().header_read_timeout(Duration::from_millis(300));
  let server = start(whoami(), limits).await;
  let addr = server.local_addr();

  // Slowloris: half a request head, then nothing.
  let mut slow = TcpStream::connect(addr).await.unwrap();
  let started = Instant::now();
  slow
    .write_all(b"GET /ip HTTP/1.1\r\nHost: te")
    .await
    .unwrap();
  let cut = closed_within(&mut slow, WAIT)
    .await
    .expect("the slow client is cut off");
  assert!(started.elapsed() >= Duration::from_millis(250));
  assert!(!cut.contains(" 200 "), "{cut}");

  // A client that never sends a byte.
  let mut silent = TcpStream::connect(addr).await.unwrap();
  assert!(closed_within(&mut silent, WAIT).await.is_some());

  // An idle keep-alive connection, after a complete request.
  let mut idle = TcpStream::connect(addr).await.unwrap();
  idle
    .write_all(get_request("/ip", "").as_bytes())
    .await
    .unwrap();
  assert!(read_response(&mut idle).await.starts_with("HTTP/1.1 200"));
  assert!(closed_within(&mut idle, WAIT).await.is_some());
  server.shutdown().await;

  // The documented opt-out leaves a silent client connected.
  let limits = ConnectionLimits::new().dangerous_no_header_read_timeout();
  let server = start(whoami(), limits).await;
  let mut silent = TcpStream::connect(server.local_addr()).await.unwrap();
  assert!(
    closed_within(&mut silent, Duration::from_millis(600))
      .await
      .is_none()
  );
  drop(silent);
  server.shutdown().await;
}

#[tokio::test]
async fn m1_connection_cap_holds() {
  let server = start(whoami(), ConnectionLimits::new().max_connections(1)).await;
  let addr = server.local_addr();

  let mut first = TcpStream::connect(addr).await.unwrap();
  first
    .write_all(get_request("/ip", "").as_bytes())
    .await
    .unwrap();
  assert!(read_response(&mut first).await.starts_with("HTTP/1.1 200"));

  // The second client waits in the backlog while the first holds the only slot ...
  let mut second = TcpStream::connect(addr).await.unwrap();
  second
    .write_all(get_request("/ip", "").as_bytes())
    .await
    .unwrap();
  let mut byte = [0_u8; 1];
  assert!(
    tokio::time::timeout(Duration::from_millis(300), second.read(&mut byte))
      .await
      .is_err(),
    "a second connection was served over the cap"
  );
  // ... and is served once it is free.
  drop(first);
  assert!(read_response(&mut second).await.starts_with("HTTP/1.1 200"));
  server.shutdown().await;
}

#[tokio::test]
async fn m1_keep_alive_limit_closes_the_connection() {
  let limits = ConnectionLimits::new().max_requests_per_connection(2);
  let server = start(whoami(), limits).await;
  let mut stream = TcpStream::connect(server.local_addr()).await.unwrap();
  let request = get_request("/ip", "");

  stream.write_all(request.as_bytes()).await.unwrap();
  let first = read_response(&mut stream).await;
  assert!(
    !first.to_ascii_lowercase().contains("connection: close"),
    "{first}"
  );
  stream.write_all(request.as_bytes()).await.unwrap();
  let second = read_response(&mut stream).await;
  assert!(
    second.to_ascii_lowercase().contains("connection: close"),
    "{second}"
  );
  assert_eq!(closed_within(&mut stream, WAIT).await.as_deref(), Some(""));
  server.shutdown().await;
}

#[tokio::test]
async fn graceful_shutdown_finishes_in_flight_requests() {
  let server = Arc::new(start(whoami(), ConnectionLimits::default()).await);
  let addr = server.local_addr();
  let mut stream = TcpStream::connect(addr).await.unwrap();
  stream
    .write_all(get_request("/slow", "").as_bytes())
    .await
    .unwrap();
  tokio::time::sleep(Duration::from_millis(50)).await;

  let stopping = Arc::clone(&server);
  let shutdown = tokio::spawn(async move { stopping.shutdown().await });
  let response = read_response(&mut stream).await;
  assert!(response.starts_with("HTTP/1.1 200"), "{response}");
  assert!(response.ends_with("done"), "{response}");
  shutdown.await.unwrap();
  assert!(server.readiness().is_shutting_down());
  assert!(TcpStream::connect(addr).await.is_err());
}
