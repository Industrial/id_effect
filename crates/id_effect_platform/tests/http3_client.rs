//! The HTTP/3 client (r12-e12-3-h3-client; ADR 0014 C15): the `Alt-Svc` upgrade, the TCP
//! fallback within 300 ms, the negative cache, and the 0.11 guards (SSRF policy, timeouts, body
//! caps, server-name verification) on the HTTP/3 path.
//!
//! The origins are in-process: an HTTP/1.1-over-TLS server on TCP (rustls on a std thread) and
//! an HTTP/3 server on the platform `QuicEndpoint` (`h3` + `h3-quinn`), both on 127.0.0.1 with
//! certificates from a throwaway CA.

#![cfg(feature = "http3")]

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use common::run;
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  Http3Client, HttpClient, HttpClientConfig, HttpClientEnv, HttpRequest, Origin, RetryPolicy,
  execute, provide_reqwest_http_client_with,
};
use id_effect_platform::quic::{EndpointStats, QuicEndpoint, QuicEndpointConfig, Route};
use id_effect_platform::tls::{Identity, NodeCertificate, server_tls_config};

/// A throwaway certificate authority.
struct Ca {
  cert: rcgen::Certificate,
  key: rcgen::KeyPair,
}

impl Ca {
  fn new() -> Self {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, "h3 test ca");
    params.key_usages = vec![
      rcgen::KeyUsagePurpose::KeyCertSign,
      rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().expect("ca key");
    let cert = params.self_signed(&key).expect("ca cert");
    Self { cert, key }
  }

  fn der(&self) -> Vec<u8> {
    self.cert.der().to_vec()
  }

  /// A server identity for `name` (a host name or an IP address).
  fn identity(&self, name: &str) -> Identity {
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).expect("params");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = params.signed_by(&key, &self.cert, &self.key).expect("leaf");
    let cert = NodeCertificate::from_pem(
      format!("{}{}", cert.pem(), self.cert.pem()).as_bytes(),
      key.serialize_pem().as_bytes(),
    )
    .expect("pem");
    Identity::from_certificate(&cert).expect("identity")
  }
}

/// An HTTP/1.1-over-TLS origin answering `tcp` with an optional `Alt-Svc` header; records when
/// each connection was accepted.
struct TcpOrigin {
  addr: SocketAddr,
  accepts: Arc<Mutex<Vec<Instant>>>,
}

impl TcpOrigin {
  fn start(identity: &Identity, alt_svc: Option<String>) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp");
    let addr = listener.local_addr().expect("addr");
    let tls = Arc::new(server_tls_config(identity, None, &[]).expect("tls"));
    let accepts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&accepts);
    std::thread::spawn(move || {
      for stream in listener.incoming().flatten() {
        seen.lock().expect("accepts").push(Instant::now());
        let tls = Arc::clone(&tls);
        let alt_svc = alt_svc.clone();
        std::thread::spawn(move || serve_tcp(stream, tls, alt_svc));
      }
    });
    Self { addr, accepts }
  }

  fn port(&self) -> u16 {
    self.addr.port()
  }

  fn accepts(&self) -> Vec<Instant> {
    self.accepts.lock().expect("accepts").clone()
  }
}

fn serve_tcp(stream: TcpStream, tls: Arc<rustls::ServerConfig>, alt_svc: Option<String>) {
  let Ok(conn) = rustls::ServerConnection::new(tls) else {
    return;
  };
  let mut s = rustls::StreamOwned::new(conn, stream);
  let mut head = Vec::new();
  let mut byte = [0u8; 1];
  while !head.ends_with(b"\r\n\r\n") {
    match s.read(&mut byte) {
      Ok(1) => head.push(byte[0]),
      _ => return,
    }
  }
  let alt = alt_svc
    .map(|v| format!("alt-svc: {v}\r\n"))
    .unwrap_or_default();
  let response =
    format!("HTTP/1.1 200 OK\r\ncontent-length: 3\r\n{alt}connection: close\r\n\r\ntcp");
  let _ = s.write_all(response.as_bytes());
  s.conn.send_close_notify();
  let _ = s.flush();
}

/// An HTTP/3 origin on the platform endpoint, presenting `identity`. Paths: `/` answers `h3`,
/// `/echo` echoes the body, `/big` sends 4096 bytes with a `content-length`,
/// `/big-undeclared` the same without one, `/slow` answers after 2 s.
fn h3_origin(identity: Identity) -> QuicEndpoint {
  let endpoint = QuicEndpoint::bind(
    "127.0.0.1:0".parse().expect("addr"),
    QuicEndpointConfig::new(identity),
  )
  .expect("bind quic");
  let route = endpoint.register(Route::h3()).expect("route");
  tokio::spawn(async move {
    while let Some(conn) = route.accept().await {
      if let Ok(conn) = conn {
        tokio::spawn(serve_h3(conn));
      }
    }
  });
  endpoint
}

type ServerStream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

async fn serve_h3(conn: quinn::Connection) {
  let Ok(mut h3) = h3::server::builder()
    .build::<_, Bytes>(h3_quinn::Connection::new(conn))
    .await
  else {
    return;
  };
  while let Ok(Some(resolver)) = h3.accept().await {
    let Ok((req, mut stream)) = resolver.resolve_request().await else {
      continue;
    };
    if req.uri().path() == "/goaway" {
      // Answer, then announce the shutdown (GOAWAY) but keep the QUIC connection open.
      respond(&mut stream, b"bye".to_vec(), true).await;
      let _ = h3.shutdown(0).await;
      tokio::time::sleep(Duration::from_secs(5)).await;
      return;
    }
    tokio::spawn(handle(req, stream));
  }
}

async fn handle(req: http::Request<()>, mut stream: ServerStream) {
  let reset = h3::error::Code::H3_INTERNAL_ERROR;
  let (body, declared) = match req.uri().path() {
    "/echo" => {
      let mut body = Vec::new();
      while let Ok(Some(mut chunk)) = stream.recv_data().await {
        body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
      }
      (body, true)
    }
    "/big" => (vec![b'x'; 4096], true),
    "/big-undeclared" => (vec![b'x'; 4096], false),
    "/slow" => {
      tokio::time::sleep(Duration::from_secs(2)).await;
      (b"late".to_vec(), true)
    }
    "/reset-head" => return stream.stop_stream(reset),
    "/reset-body" | "/stall" => {
      let head = http::Response::builder()
        .status(200)
        .body(())
        .expect("head");
      if stream.send_response(head).await.is_ok()
        && stream.send_data(Bytes::from_static(b"x")).await.is_ok()
      {
        if req.uri().path() == "/stall" {
          tokio::time::sleep(Duration::from_secs(3)).await;
        } else {
          // Let the head and the first chunk arrive, then reset mid-body.
          tokio::time::sleep(Duration::from_millis(150)).await;
          stream.stop_stream(reset);
        }
      }
      return;
    }
    _ => (b"h3".to_vec(), true),
  };
  respond(&mut stream, body, declared).await;
}

async fn respond(stream: &mut ServerStream, body: Vec<u8>, declared: bool) {
  let mut resp = http::Response::builder().status(200);
  if declared {
    resp = resp.header("content-length", body.len());
  }
  if stream
    .send_response(resp.body(()).expect("response"))
    .await
    .is_err()
  {
    return;
  }
  for chunk in body.chunks(1024) {
    if stream
      .send_data(Bytes::copy_from_slice(chunk))
      .await
      .is_err()
    {
      return;
    }
  }
  let _ = stream.finish().await;
}

/// The client config for the local origins: loopback allowed, the test CA trusted, HTTP/3 on,
/// no retries.
fn local(ca: &Ca) -> HttpClientConfig {
  HttpClientConfig::default()
    .allow_hosts(["127.0.0.1"])
    .with_retry(RetryPolicy::none())
    .add_root_certificate_der(ca.der())
    .http3(true)
}

fn url(port: u16, path: &str) -> String {
  format!("https://127.0.0.1:{port}{path}")
}

async fn get(client: &Http3Client, url: String) -> Result<Vec<u8>, HttpError> {
  run(client.execute(HttpRequest::get(url)))
    .await
    .map(|r| r.body)
}

#[tokio::test(flavor = "multi_thread")]
async fn alt_svc_upgrades_next_request_to_h3() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("127.0.0.1"));
  let tcp = TcpOrigin::start(
    &ca.identity("127.0.0.1"),
    Some(format!("h3=\":{}\"; ma=60", h3.local_addr().port())),
  );
  let client = Http3Client::from_config(&local(&ca));
  assert!(client.http3_available());

  // First request: TCP, which teaches the alternative.
  assert_eq!(
    get(&client, url(tcp.port(), "/")).await.expect("tcp"),
    b"tcp"
  );
  let origin = Origin::new("127.0.0.1", tcp.port());
  assert_eq!(
    client.alt_svc().lookup(&origin, Instant::now()),
    Some(h3.local_addr().port())
  );
  // Next requests: HTTP/3 on one pooled connection, with bodies both ways.
  assert_eq!(get(&client, url(tcp.port(), "/")).await.expect("h3"), b"h3");
  let mut post = HttpRequest::post(url(tcp.port(), "/echo"), b"ping".to_vec());
  post.headers = vec![
    ("connection".into(), "keep-alive".into()),
    ("x-test".into(), "1".into()),
  ];
  assert_eq!(run(client.execute(post)).await.expect("echo").body, b"ping");
  let streamed = run(client.execute_stream(HttpRequest::get(url(tcp.port(), "/big"))))
    .await
    .expect("stream");
  assert_eq!(streamed.status, 200);
  assert_eq!(
    run(streamed.body.collect()).await.expect("body").len(),
    4096
  );
  assert_eq!(tcp.accepts().len(), 1, "only the first request used TCP");
  assert_eq!(h3.stats().accepted, 1, "one pooled HTTP/3 connection");
  assert!(format!("{client:?}").contains("alt_svc_entries: 1"));
}

/// An HTTP/2-over-TLS origin (ALPN `h2` only) answering `tcp` with an `Alt-Svc` header; records
/// when each request arrived and its HTTP version.
struct H2Origin {
  addr: SocketAddr,
  requests: Arc<Mutex<Vec<(Instant, http::Version)>>>,
}

impl H2Origin {
  async fn start(identity: &Identity, alt_svc: String) -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .expect("bind tcp");
    let addr = listener.local_addr().expect("addr");
    let mut tls = server_tls_config(identity, None, &[]).expect("tls");
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    tokio::spawn(async move {
      while let Ok((stream, _)) = listener.accept().await {
        let (acceptor, seen, alt_svc) = (acceptor.clone(), Arc::clone(&seen), alt_svc.clone());
        tokio::spawn(async move {
          let Ok(tls) = acceptor.accept(stream).await else {
            return;
          };
          let service = hyper::service::service_fn(move |req: http::Request<_>| {
            seen
              .lock()
              .expect("requests")
              .push((Instant::now(), req.version()));
            let response = http::Response::builder()
              .header("alt-svc", alt_svc.as_str())
              .body(http_body_util::Full::new(Bytes::from_static(b"tcp")))
              .expect("response");
            async move { Ok::<_, std::convert::Infallible>(response) }
          });
          let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .serve_connection(hyper_util::rt::TokioIo::new(tls), service)
            .await;
        });
      }
    });
    Self { addr, requests }
  }

  fn port(&self) -> u16 {
    self.addr.port()
  }

  fn requests(&self) -> Vec<(Instant, http::Version)> {
    self.requests.lock().expect("requests").clone()
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn udp_blackhole_falls_back_to_h2_within_budget() {
  let ca = Ca::new();
  // A UDP port that never answers.
  let blackhole = UdpSocket::bind("127.0.0.1:0").expect("bind udp");
  let tcp = H2Origin::start(
    &ca.identity("127.0.0.1"),
    format!("h3=\":{}\"", blackhole.local_addr().expect("addr").port()),
  )
  .await;
  let client = Http3Client::from_config(&local(&ca));
  assert_eq!(
    get(&client, url(tcp.port(), "/")).await.expect("first"),
    b"tcp"
  );

  let started = Instant::now();
  assert_eq!(
    get(&client, url(tcp.port(), "/")).await.expect("fallback"),
    b"tcp"
  );
  let elapsed = started.elapsed();
  let requests = tcp.requests();
  assert_eq!(requests.len(), 2);
  // The TCP path is HTTP/2 (reqwest `http2`, ALPN `h2`), both times.
  assert!(
    requests.iter().all(|(_, v)| *v == http::Version::HTTP_2),
    "{requests:?}"
  );
  let delay = requests[1].0.duration_since(started);
  assert!(
    delay >= Duration::from_millis(290),
    "HTTP/3 was tried first ({delay:?})"
  );
  assert!(
    delay <= Duration::from_millis(300 + 200),
    "the TCP request waited {delay:?}, more than the 300 ms fallback timer"
  );
  assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn negative_cache_skips_h3_after_failure() {
  let ca = Ca::new();
  let blackhole = UdpSocket::bind("127.0.0.1:0").expect("bind udp");
  let tcp = TcpOrigin::start(
    &ca.identity("127.0.0.1"),
    Some(format!(
      "h3=\":{}\"",
      blackhole.local_addr().expect("addr").port()
    )),
  );
  let client = Http3Client::from_config(&local(&ca));
  let origin = Origin::new("127.0.0.1", tcp.port());
  get(&client, url(tcp.port(), "/")).await.expect("learn");
  get(&client, url(tcp.port(), "/")).await.expect("fallback");
  assert!(client.alt_svc().is_negative(&origin, Instant::now()));
  assert_eq!(client.alt_svc().lookup(&origin, Instant::now()), None);

  // The origin still advertises h3, but it is not tried again: no 300 ms wait.
  let started = Instant::now();
  assert_eq!(
    get(&client, url(tcp.port(), "/")).await.expect("skip"),
    b"tcp"
  );
  let accepts = tcp.accepts();
  assert_eq!(accepts.len(), 3);
  assert!(
    accepts[2].duration_since(started) < Duration::from_millis(200),
    "HTTP/3 was tried although the origin is in the negative cache"
  );
  // The exclusion lasts the negative TTL (5 minutes by default), then HTTP/3 is tried again.
  let later = Instant::now() + Duration::from_secs(5 * 60);
  assert!(!client.alt_svc().is_negative(&origin, later));
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_path_enforces_ssrf_policy() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("localhost"));
  let tcp = TcpOrigin::start(&ca.identity("localhost"), None);
  let quic_port = h3.local_addr().port();
  // The default policy: loopback refused, no exemption.
  let guarded = Http3Client::from_config(
    &HttpClientConfig::default()
      .with_retry(RetryPolicy::none())
      .add_root_certificate_der(ca.der())
      .http3(true),
  );
  let refused =
    |r: Result<_, HttpError>| matches!(r, Err(HttpError::InvalidRequest(m)) if m.contains("SSRF"));

  // A literal private address: refused by the URL check.
  let literal = run(guarded.execute_h3(HttpRequest::get(url(quic_port, "/")), quic_port)).await;
  assert!(refused(literal.map(|r| r.body)));
  // A host name: every resolved address is checked before a packet is sent.
  let named = format!("https://localhost:{quic_port}/");
  assert!(refused(
    run(guarded.execute_h3(HttpRequest::get(named), quic_port))
      .await
      .map(|r| r.body)
  ));
  // A cached alternative does not bypass the policy, and the request is not sent over TCP.
  let origin = Origin::new("localhost", tcp.port());
  guarded
    .alt_svc()
    .insert(&origin, quic_port, Duration::from_secs(60), Instant::now());
  let cached = format!("https://localhost:{}/", tcp.port());
  assert!(refused(get(&guarded, cached).await));

  tokio::time::sleep(Duration::from_millis(100)).await;
  assert_eq!(
    h3.stats(),
    EndpointStats::default(),
    "no QUIC packet reached the origin"
  );
  assert!(tcp.accepts().is_empty(), "no TCP connection either");

  // Control: with localhost allowed, the same request succeeds over HTTP/3.
  let allowed = Http3Client::from_config(&local(&ca).allow_hosts(["localhost"]));
  let ok = run(allowed.execute_h3(
    HttpRequest::get(format!("https://localhost:{quic_port}/")),
    quic_port,
  ))
  .await;
  match ok {
    Ok(resp) => assert_eq!(resp.body, b"h3"),
    // localhost may resolve to ::1 first, where nothing listens; the policy still passed.
    Err(HttpError::InvalidRequest(m)) => assert!(m.starts_with("http3:"), "{m}"),
    Err(e) => panic!("unexpected {e:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_body_cap_enforced() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("127.0.0.1"));
  let port = h3.local_addr().port();
  let client = Http3Client::from_config(&local(&ca));
  let capped = |path: &str, max: usize| HttpRequest {
    max_body_bytes: Some(max),
    ..HttpRequest::get(url(port, path))
  };

  // A declared length over the cap fails before the body is read.
  let declared = run(client.execute_h3(capped("/big", 1000), port)).await;
  assert!(
    matches!(
      declared,
      Err(HttpError::BodyTooLarge {
        len: 4096,
        max: 1000
      })
    ),
    "{declared:?}"
  );
  // Without a declared length the running total is capped.
  let running = run(client.execute_h3(capped("/big-undeclared", 1000), port)).await;
  assert!(
    matches!(running, Err(HttpError::BodyTooLarge { len, max: 1000 }) if len > 1000 && len <= 2048),
    "{running:?}"
  );
  // At the cap it passes.
  let fits = run(client.execute_h3(capped("/big-undeclared", 4096), port))
    .await
    .expect("fits");
  assert_eq!(fits.body.len(), 4096);

  // Streaming over a cached alternative: the cap holds chunk by chunk.
  client.alt_svc().insert(
    &Origin::new("127.0.0.1", port),
    port,
    Duration::from_secs(60),
    Instant::now(),
  );
  let streamed = run(client.execute_stream(capped("/big-undeclared", 1000)))
    .await
    .expect("head");
  let body = run(streamed.body.collect()).await;
  assert!(
    matches!(body, Err(HttpError::BodyTooLarge { max: 1000, .. })),
    "{body:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_rejects_wrong_server_name() {
  let ca = Ca::new();
  // The HTTP/3 origin presents a certificate for another name; the TCP origin a correct one.
  let h3 = h3_origin(ca.identity("other.test"));
  let tcp = TcpOrigin::start(&ca.identity("127.0.0.1"), None);
  let quic_port = h3.local_addr().port();
  let client = Http3Client::from_config(&local(&ca));

  let direct = run(client.execute_h3(HttpRequest::get(url(quic_port, "/")), quic_port)).await;
  assert!(
    matches!(&direct, Err(HttpError::InvalidRequest(m)) if m.starts_with("http3: handshake")),
    "{direct:?}"
  );
  // Over a cached alternative the request falls back to TCP and the origin is excluded.
  let origin = Origin::new("127.0.0.1", tcp.port());
  client
    .alt_svc()
    .insert(&origin, quic_port, Duration::from_secs(60), Instant::now());
  assert_eq!(
    get(&client, url(tcp.port(), "/")).await.expect("tcp"),
    b"tcp"
  );
  assert!(client.alt_svc().is_negative(&origin, Instant::now()));
  assert_eq!(h3.stats().accepted, 0, "no HTTP/3 connection was completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_request_timeout_applies() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("127.0.0.1"));
  let port = h3.local_addr().port();
  let client = Http3Client::from_config(&local(&ca).with_timeout(Duration::from_millis(400)));
  let started = Instant::now();
  let slow = run(client.execute_h3(HttpRequest::get(url(port, "/slow")), port)).await;
  assert!(
    matches!(&slow, Err(HttpError::InvalidRequest(m)) if m.contains("timed out")),
    "{slow:?}"
  );
  assert!(started.elapsed() < Duration::from_millis(1500));
  // A request's own timeout replaces the client's.
  let req = HttpRequest {
    timeout: Some(Duration::from_secs(5)),
    ..HttpRequest::get(url(port, "/slow"))
  };
  assert_eq!(
    run(client.execute_h3(req, port)).await.expect("slow").body,
    b"late"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_https_and_disabled_clients_stay_on_tcp() {
  let ca = Ca::new();
  let client = Http3Client::from_config(&local(&ca));
  // execute_h3 needs an https URL.
  let plain = run(client.execute_h3(HttpRequest::get("http://127.0.0.1:1/"), 1)).await;
  assert!(matches!(plain, Err(HttpError::InvalidRequest(m)) if m.contains("only https")));
  let bad = run(client.execute_h3(HttpRequest::get("not a url"), 1)).await;
  assert!(matches!(bad, Err(HttpError::InvalidRequest(m)) if m.contains("not a URL")));
  // A disabled client never uses HTTP/3.
  let off = Http3Client::from_config(&local(&ca).http3(false));
  assert!(!off.http3_available());
  let refused = run(off.execute_h3(HttpRequest::get(url(1, "/")), 1)).await;
  assert!(matches!(refused, Err(HttpError::InvalidRequest(m)) if m.contains("HTTP/3 is off")));
  // The layer builds the HTTP/3 client when the config turns it on.
  let tcp = TcpOrigin::start(&ca.identity("127.0.0.1"), None);
  let program = execute::<HttpClientEnv, _>(HttpRequest::get(url(tcp.port(), "/")))
    .provide_layer(provide_reqwest_http_client_with(local(&ca)));
  assert_eq!(run(program).await.expect("layer").body, b"tcp");
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_body_errors_and_stalls_fail_the_request() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("127.0.0.1"));
  let port = h3.local_addr().port();
  let client = Http3Client::from_config(&local(&ca).with_timeout(Duration::from_millis(500)));
  let reset = run(client.execute_h3(HttpRequest::get(url(port, "/reset-body")), port)).await;
  assert!(
    matches!(&reset, Err(HttpError::InvalidRequest(m)) if m.starts_with("http3: response body:")),
    "{reset:?}"
  );
  let stalled = run(client.execute_h3(HttpRequest::get(url(port, "/stall")), port)).await;
  assert!(
    matches!(&stalled, Err(HttpError::InvalidRequest(m)) if m.contains("response body timed out")),
    "{stalled:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reset_request_falls_back_only_when_idempotent() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("127.0.0.1"));
  let tcp = TcpOrigin::start(&ca.identity("127.0.0.1"), None);
  let origin = Origin::new("127.0.0.1", tcp.port());
  let seeded = || {
    let client = Http3Client::from_config(&local(&ca));
    client.alt_svc().insert(
      &origin,
      h3.local_addr().port(),
      Duration::from_secs(60),
      Instant::now(),
    );
    client
  };
  // A GET the server reset is sent again over TCP.
  let client = seeded();
  assert_eq!(
    get(&client, url(tcp.port(), "/reset-head"))
      .await
      .expect("get"),
    b"tcp"
  );
  assert!(client.alt_svc().is_negative(&origin, Instant::now()));
  // A POST may have reached the server: it fails instead of being repeated.
  let client = seeded();
  let post = HttpRequest::post(url(tcp.port(), "/reset-head"), b"once".to_vec());
  let failed = run(client.execute(post)).await;
  assert!(
    matches!(&failed, Err(HttpError::InvalidRequest(m)) if m.starts_with("http3: request")),
    "{failed:?}"
  );
  assert_eq!(tcp.accepts().len(), 1, "the POST was not sent over TCP");
}

#[tokio::test(flavor = "multi_thread")]
async fn pooled_connections_are_replaced_after_goaway_or_close() {
  let ca = Ca::new();
  let h3 = h3_origin(ca.identity("127.0.0.1"));
  let port = h3.local_addr().port();
  let client = Http3Client::from_config(&local(&ca));
  // After a GOAWAY the pooled connection cannot open streams: one new connection is dialed.
  let bye = run(client.execute_h3(HttpRequest::get(url(port, "/goaway")), port)).await;
  assert_eq!(bye.expect("goaway").body, b"bye");
  tokio::time::sleep(Duration::from_millis(200)).await;
  let next = run(client.execute_h3(HttpRequest::get(url(port, "/")), port)).await;
  assert_eq!(next.expect("redial").body, b"h3");
  assert_eq!(h3.stats().accepted, 2);
  // A closed connection is dropped from the pool; the closed endpoint then answers nothing.
  h3.close();
  tokio::time::sleep(Duration::from_millis(200)).await;
  let closed = run(client.execute_h3(HttpRequest::get(url(port, "/")), port)).await;
  assert!(
    matches!(&closed, Err(HttpError::InvalidRequest(m)) if m.starts_with("http3:")),
    "{closed:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn tcp_errors_pass_through() {
  let ca = Ca::new();
  let client = Http3Client::from_config(&local(&ca));
  for target in ["https://127.0.0.1:1/", "http://127.0.0.1:1/"] {
    let err = get(&client, target.to_string()).await;
    assert!(
      matches!(&err, Err(HttpError::Reqwest(e)) if e.is_connect()),
      "{err:?}"
    );
  }
}

/// The build never uses reqwest's own HTTP/3 (ADR 0014 R5): its cfg flag appears in no
/// manifest, source, workflow or Nix file of the workspace.
#[test]
fn workspace_never_sets_the_reqwest_http3_cfg() {
  let flag = concat!("reqwest", "_unstable");
  let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
  let mut stack = vec![root];
  let mut hits = Vec::new();
  while let Some(dir) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&dir) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      let name = entry.file_name().to_string_lossy().into_owned();
      if path.is_dir() {
        if !name.starts_with('.') && name != "target" && name != "node_modules" {
          stack.push(path);
        }
      } else if [".toml", ".rs", ".yml", ".nix"]
        .iter()
        .any(|ext| name.ends_with(ext))
        && std::fs::read_to_string(&path).is_ok_and(|s| s.contains(flag))
      {
        hits.push(path);
      }
    }
  }
  // `.cargo/config.toml` is where a rustflags cfg would live.
  let cargo_config = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cargo");
  for file in ["config.toml", "config"] {
    if std::fs::read_to_string(cargo_config.join(file)).is_ok_and(|s| s.contains(flag)) {
      hits.push(cargo_config.join(file));
    }
  }
  assert!(hits.is_empty(), "{flag} found in {hits:?}");
}
