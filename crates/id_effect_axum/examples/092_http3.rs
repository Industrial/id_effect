//! 092 — one router over HTTP/3, HTTP/2 and HTTP/1.1 (feature `http3`).
//!
//! The host serves TLS on TCP (ALPN `h2`, `http/1.1`) and HTTP/3 on UDP with the same port
//! number and the same certificate; every TCP response carries `Alt-Svc: h3=":<port>"; ma=86400`,
//! so browsers and the platform `Http3Client` upgrade on their next request. `HTTP3=false`
//! turns HTTP/3 off. Without `TLS_CERT_FILE` / `TLS_KEY_FILE` the example writes a self-signed
//! `localhost` certificate to a temporary directory.
//!
//! ```text
//! cargo run -p id_effect_axum --features http3 --example 092_http3               # :8443
//! PORT=4433 cargo run -p id_effect_axum --features http3 --example 092_http3
//! cargo run -p id_effect_axum --features http3 --example 092_http3 -- --self-test
//! curl --http3-only -k https://localhost:8443/hello
//! ```

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use bytes::{Buf, Bytes};
use id_effect_axum::server::HostTls;
use id_effect_axum::{HostConfig, ServerHandle};
use id_effect_platform::quic::{ALPN_H3, QuicLimits, client_config};
use id_effect_platform::tls::provider;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};

fn app() -> Router {
  Router::new()
    .route("/hello", get(|| async { "hello over any HTTP version\n" }))
    .route(
      "/version",
      get(|request: axum::extract::Request| async move { format!("{:?}\n", request.version()) }),
    )
}

/// The TLS settings: the files from the environment, or a fresh self-signed certificate.
fn tls() -> (HostTls, Option<Vec<u8>>) {
  if let (Ok(cert), Ok(key)) = (
    std::env::var("TLS_CERT_FILE"),
    std::env::var("TLS_KEY_FILE"),
  ) {
    return (HostTls::files(cert, key), None);
  }
  let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("rcgen");
  let dir = std::env::temp_dir().join(format!("id-effect-092-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("temp dir");
  let (chain, key) = (dir.join("chain.pem"), dir.join("key.pem"));
  std::fs::write(&chain, ck.cert.pem()).expect("write chain");
  std::fs::write(&key, ck.key_pair.serialize_pem()).expect("write key");
  (HostTls::files(chain, key), Some(ck.cert.der().to_vec()))
}

fn roots(der: &[u8]) -> RootCertStore {
  let mut roots = RootCertStore::empty();
  roots.add(der.to_vec().into()).expect("root");
  roots
}

/// GET `/version` over HTTP/1.1 on TLS: the response must advertise HTTP/3.
async fn tcp_self_test(addr: SocketAddr, der: &[u8]) {
  let mut config = ClientConfig::builder_with_provider(provider())
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_root_certificates(roots(der))
    .with_no_client_auth();
  config.alpn_protocols = vec![b"http/1.1".to_vec()];
  let tcp = tokio::net::TcpStream::connect(addr).await.expect("tcp");
  let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
    .connect("localhost".try_into().expect("name"), tcp)
    .await
    .expect("tls");
  tls
    .write_all(b"GET /version HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
    .await
    .expect("request");
  let mut response = String::new();
  let _ = tls.read_to_string(&mut response).await;
  let expected = format!("alt-svc: h3=\":{}\"; ma=86400", addr.port());
  assert!(response.starts_with("HTTP/1.1 200"), "{response}");
  assert!(response.contains(&expected), "{response}");
  assert!(response.contains("HTTP/1.1"), "{response}");
}

/// GET `/version` over HTTP/3 on the same port number.
async fn h3_self_test(addr: SocketAddr, der: &[u8]) {
  let tls = ClientConfig::builder_with_provider(provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls 1.3")
    .with_root_certificates(roots(der))
    .with_no_client_auth();
  let config = client_config(tls, &[ALPN_H3], &QuicLimits::default()).expect("quic config");
  let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("udp");
  let conn = endpoint
    .connect_with(config, addr, "localhost")
    .expect("connect")
    .await
    .expect("handshake");
  let (mut driver, mut send) = h3::client::new(h3_quinn::Connection::new(conn))
    .await
    .expect("h3");
  tokio::spawn(async move {
    let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
  });
  let request = axum::http::Request::get("https://localhost/version")
    .body(())
    .expect("request");
  let mut stream = send.send_request(request).await.expect("send");
  stream.finish().await.expect("finish");
  let response = stream.recv_response().await.expect("response");
  assert_eq!(response.status(), 200);
  let mut body = Vec::new();
  while let Some(mut chunk) = stream.recv_data().await.expect("body") {
    body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
  }
  assert_eq!(Bytes::from(body), Bytes::from_static(b"HTTP/3.0\n"));
}

#[tokio::main]
async fn main() {
  let self_test = std::env::args().any(|a| a == "--self-test");
  let port = std::env::var("PORT")
    .ok()
    .and_then(|p| p.parse().ok())
    .unwrap_or(if self_test { 0 } else { 8443 });
  let (tls, self_signed) = tls();
  let config = HostConfig {
    bind_host: std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
    bind_port: port,
    shutdown_timeout: Duration::from_secs(5),
    http3: std::env::var("HTTP3").map_or(true, |v| v != "false"),
    ..HostConfig::default()
  }
  .with_tls(tls);
  let server = ServerHandle::start(&config, app()).await.expect("start");
  let addr = server.local_addr();
  if self_test {
    let der = self_signed.expect("--self-test uses the self-signed certificate");
    tcp_self_test(addr, &der).await;
    h3_self_test(addr, &der).await;
    server.shutdown().await;
    println!("092_http3 self-test ok");
    return;
  }
  println!(
    "serving https://localhost:{} over TCP (h2, http/1.1) and UDP (h3)",
    addr.port()
  );
  let _ = tokio::signal::ctrl_c().await;
  server.shutdown().await;
}
