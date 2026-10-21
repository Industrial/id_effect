//! HTTP/3 settings (r13-f-h3-settings): real 0-RTT on the `h3` route behind
//! `Http3Options::dangerous_allow_zero_rtt_for_queries` (F25: safe methods served from early
//! data, everything else `425 Too Early`) and the congestion-control switch (F36).
//!
//! A UDP relay between client and server holds back the server's handshake flight (its
//! Handshake and 1-RTT packets) on demand: the client cannot complete the handshake and sends
//! its requests in 0-RTT, so they provably reach the server while the handshake is running.
#![cfg(feature = "http3")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use bytes::{Buf, Bytes, BytesMut};
use id_effect_axum::ConnectionLimits;
use id_effect_axum::server::h3::{CONGESTION_KEY, Http3Options, quic_limits, serve_h3};
use id_effect_config::{MapConfigProvider, ProviderOptions};
use id_effect_platform::quic::{
  ALPN_H3, CongestionControl, QuicEndpoint, QuicEndpointConfig, QuicLimits, transport_config,
};
use id_effect_platform::tls::{Identity, NodeCertificate, client_tls_config};
use tokio::net::UdpSocket;
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(20);

fn local() -> SocketAddr {
  "127.0.0.1:0".parse().unwrap()
}

// --- a relay that can hold the server's handshake flight ----------------------------------------

/// Relays one client's datagrams to `server` and back; while held, server datagrams carrying a
/// Handshake or 1-RTT packet wait, so the client cannot complete the handshake.
struct Relay {
  addr: SocketAddr,
  hold: Arc<AtomicBool>,
  released: Arc<Notify>,
  task: tokio::task::JoinHandle<()>,
}

impl Drop for Relay {
  fn drop(&mut self) {
    self.task.abort();
  }
}

impl Relay {
  async fn start(server: SocketAddr) -> Self {
    let socket = UdpSocket::bind(local()).await.unwrap();
    let addr = socket.local_addr().unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let released = Arc::new(Notify::new());
    let task = tokio::spawn(relay(
      socket,
      server,
      Arc::clone(&hold),
      Arc::clone(&released),
    ));
    Self {
      addr,
      hold,
      released,
      task,
    }
  }

  fn hold(&self) {
    self.hold.store(true, Ordering::SeqCst);
  }

  fn release(&self) {
    self.hold.store(false, Ordering::SeqCst);
    self.released.notify_one();
  }
}

async fn relay(
  socket: UdpSocket,
  server: SocketAddr,
  hold: Arc<AtomicBool>,
  released: Arc<Notify>,
) {
  let mut client = None;
  let mut held: Vec<Vec<u8>> = Vec::new();
  let mut buf = vec![0u8; 65_536];
  loop {
    tokio::select! {
      received = socket.recv_from(&mut buf) => {
        let Ok((n, from)) = received else { return };
        let datagram = &buf[..n];
        if from != server {
          client = Some(from);
          let _ = socket.send_to(datagram, server).await;
        } else if let Some(client) = client {
          if hold.load(Ordering::SeqCst) && carries_handshake(datagram) {
            held.push(datagram.to_vec());
          } else {
            let _ = socket.send_to(datagram, client).await;
          }
        }
      }
      () = released.notified() => {
        if let Some(client) = client {
          for datagram in held.drain(..) {
            let _ = socket.send_to(&datagram, client).await;
          }
        }
      }
    }
  }
}

/// A QUIC variable-length integer and its length (RFC 9000 §16).
fn varint(bytes: &[u8]) -> Option<(usize, usize)> {
  let first = *bytes.first()?;
  let len = 1usize << (first >> 6);
  let mut value = u64::from(first & 0x3f);
  for byte in bytes.get(1..len)? {
    value = (value << 8) | u64::from(*byte);
  }
  Some((usize::try_from(value).ok()?, len))
}

/// Whether a datagram carries a Handshake or a 1-RTT packet (coalesced long-header packets
/// are walked, RFC 9000 §12.2, §17.2; a Retry carries neither).
fn carries_handshake(mut datagram: &[u8]) -> bool {
  while let Some(&first) = datagram.first() {
    if first & 0x80 == 0 {
      return true;
    }
    let kind = (first >> 4) & 0x03;
    match kind {
      0b10 => return true,
      0b11 => return false,
      _ => {}
    }
    let mut at = 5;
    let Some(&dcid) = datagram.get(at) else {
      return false;
    };
    at += 1 + usize::from(dcid);
    let Some(&scid) = datagram.get(at) else {
      return false;
    };
    at += 1 + usize::from(scid);
    if kind == 0b00 {
      let Some((token, n)) = datagram.get(at..).and_then(varint) else {
        return false;
      };
      at += n + token;
    }
    let Some((length, n)) = datagram.get(at..).and_then(varint) else {
      return false;
    };
    let Some(rest) = datagram.get(at + n + length..) else {
      return false;
    };
    datagram = rest;
  }
  false
}

// --- server and client -------------------------------------------------------------------------

/// How often each handler ran.
#[derive(Clone, Default)]
struct Ran {
  reads: Arc<AtomicUsize>,
  writes: Arc<AtomicUsize>,
  deletes: Arc<AtomicUsize>,
}

fn app(ran: &Ran) -> Router {
  let (reads, writes, deletes) = (
    Arc::clone(&ran.reads),
    Arc::clone(&ran.writes),
    Arc::clone(&ran.deletes),
  );
  Router::new().route(
    "/item",
    get(move || {
      let reads = Arc::clone(&reads);
      async move {
        reads.fetch_add(1, Ordering::SeqCst);
        "item"
      }
    })
    .post(move || {
      let writes = Arc::clone(&writes);
      async move {
        writes.fetch_add(1, Ordering::SeqCst);
        "written"
      }
    })
    .delete(move || {
      let deletes = Arc::clone(&deletes);
      async move {
        deletes.fetch_add(1, Ordering::SeqCst);
        "deleted"
      }
    }),
  )
}

/// An HTTP/3 server on its own endpoint, with `options`.
struct Server {
  endpoint: QuicEndpoint,
  stop: Option<tokio::sync::oneshot::Sender<()>>,
  task: tokio::task::JoinHandle<std::io::Result<()>>,
  cert: NodeCertificate,
  identity: Identity,
}

impl Server {
  async fn start(app: Router, options: Http3Options) -> Self {
    let cert = NodeCertificate::self_signed("localhost").unwrap();
    let identity = Identity::from_certificate(&cert).unwrap();
    let limits = ConnectionLimits::default();
    let endpoint = QuicEndpoint::bind(
      local(),
      QuicEndpointConfig::new(identity.clone()).with_limits(quic_limits(&limits)),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = {
      let endpoint = endpoint.clone();
      tokio::spawn(async move {
        serve_h3(&endpoint, app, limits, options, async {
          let _ = stopped.await;
        })
        .await
      })
    };
    // The server task registers the route: wait for it.
    tokio::time::timeout(WAIT, async {
      while endpoint.router().alpns().is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    })
    .await
    .unwrap();
    Self {
      endpoint,
      stop: Some(stop),
      task,
      cert,
      identity,
    }
  }

  /// A client config asking for early data; clones share its session cache.
  fn early_client(&self) -> quinn::ClientConfig {
    let mut tls = client_tls_config(&self.identity, None, &[self.cert.cert_sha256()]).unwrap();
    tls.enable_early_data = true;
    tls.alpn_protocols = vec![ALPN_H3.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config(&QuicLimits::default()).unwrap()));
    config
  }

  async fn stop(mut self) {
    let _ = self.stop.take().unwrap().send(());
    tokio::time::timeout(WAIT, self.task)
      .await
      .unwrap()
      .unwrap()
      .unwrap();
  }
}

type SendRequest = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// An HTTP/3 client on `conn` (which may still be in 0-RTT).
async fn h3_client(conn: quinn::Connection) -> SendRequest {
  let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(conn))
    .await
    .unwrap();
  tokio::spawn(async move {
    let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
  });
  send
}

/// One request on `/item`: its status and body.
async fn request(
  mut send: SendRequest,
  method: Method,
  body: Option<&str>,
) -> (StatusCode, String) {
  let request = Request::builder()
    .method(method)
    .uri("https://localhost/item")
    .body(())
    .unwrap();
  let exchange = async {
    let mut stream = send.send_request(request).await.unwrap();
    if let Some(body) = body {
      stream
        .send_data(Bytes::copy_from_slice(body.as_bytes()))
        .await
        .unwrap();
    }
    stream.finish().await.unwrap();
    let response = stream.recv_response().await.unwrap();
    let mut bytes = BytesMut::new();
    while let Some(mut chunk) = stream.recv_data().await.unwrap() {
      bytes.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    (
      response.status(),
      String::from_utf8(bytes.to_vec()).unwrap(),
    )
  };
  tokio::time::timeout(WAIT, exchange)
    .await
    .expect("response in time")
}

/// [`request`] in its own task, sent at once (so in 0-RTT while the relay holds).
fn spawn_request(
  send: &SendRequest,
  method: Method,
  body: Option<&'static str>,
) -> tokio::task::JoinHandle<(StatusCode, String)> {
  tokio::spawn(request(send.clone(), method, body))
}

/// A first, full handshake through `relay` (the client keeps the ticket), then a second dial
/// in 0-RTT with the server's handshake flight held back.
async fn resumed_in_0rtt(
  client: &quinn::Endpoint,
  config: &quinn::ClientConfig,
  relay: &Relay,
) -> (quinn::Connection, SendRequest) {
  let first = tokio::time::timeout(
    WAIT,
    client
      .connect_with(config.clone(), relay.addr, "localhost")
      .unwrap(),
  )
  .await
  .unwrap()
  .unwrap();
  let send = h3_client(first.clone()).await;
  assert_eq!(request(send, Method::GET, None).await.0, StatusCode::OK);
  tokio::time::sleep(Duration::from_millis(50)).await;
  first.close(0u32.into(), b"done");

  relay.hold();
  let connecting = client
    .connect_with(config.clone(), relay.addr, "localhost")
    .unwrap();
  let Ok((conn, _accepted)) = connecting.into_0rtt() else {
    panic!("the ticket allows early data");
  };
  let send = h3_client(conn.clone()).await;
  (conn, send)
}

/// Waits until `counter` reaches `n`.
async fn reaches(counter: &AtomicUsize, n: usize) {
  tokio::time::timeout(WAIT, async {
    while counter.load(Ordering::SeqCst) < n {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("handler ran");
}

// --- tests -------------------------------------------------------------------------------------

/// Off by default: `serve_h3` with the default options registers an `h3` route without early
/// data, so a resumed client cannot send 0-RTT.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_rtt_off_by_default() {
  let ran = Ran::default();
  let server = Server::start(app(&ran), Http3Options::default()).await;
  assert_eq!(server.endpoint.router().early_data(ALPN_H3), Some(false));
  let client = quinn::Endpoint::client(local()).unwrap();
  let config = server.early_client();
  let first = client
    .connect_with(config.clone(), server.endpoint.local_addr(), "localhost")
    .unwrap()
    .await
    .unwrap();
  let send = h3_client(first.clone()).await;
  assert_eq!(request(send, Method::GET, None).await.0, StatusCode::OK);
  tokio::time::sleep(Duration::from_millis(50)).await;
  let second = client
    .connect_with(config, server.endpoint.local_addr(), "localhost")
    .unwrap();
  assert!(second.into_0rtt().is_err(), "the server offers no 0-RTT");
  assert_eq!(server.endpoint.stats().early_handovers, 0);
  first.close(0u32.into(), b"done");
  server.stop().await;
}

/// With the opt-out, requests that arrive before the handshake completes are served when
/// their method is safe (`GET`, `HEAD`) and refused `425` otherwise (`OPTIONS`, `DELETE`);
/// once the handshake completed, every method is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_rtt_admits_only_safe_methods() {
  let ran = Ran::default();
  let server = Server::start(
    app(&ran),
    Http3Options::new().dangerous_allow_zero_rtt_for_queries(),
  )
  .await;
  assert_eq!(server.endpoint.router().early_data(ALPN_H3), Some(true));
  let relay = Relay::start(server.endpoint.local_addr()).await;
  let client = quinn::Endpoint::client(local()).unwrap();
  let (conn, send) = resumed_in_0rtt(&client, &server.early_client(), &relay).await;

  let early = [Method::GET, Method::HEAD, Method::OPTIONS, Method::DELETE]
    .map(|method| spawn_request(&send, method, None));
  // GET and HEAD reach the handler while the handshake is held (1 read was the first dial).
  reaches(&ran.reads, 3).await;
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(ran.deletes.load(Ordering::SeqCst), 0);
  assert_eq!(server.endpoint.stats().handshakes_failed, 0);

  relay.release();
  let mut answers = Vec::new();
  for answer in early {
    answers.push(answer.await.unwrap());
  }
  assert_eq!(answers[0], (StatusCode::OK, "item".into()));
  assert_eq!(answers[1].0, StatusCode::OK);
  for (status, body) in &answers[2..] {
    assert_eq!(*status, StatusCode::TOO_EARLY);
    assert!(body.contains("early data"), "{body}");
  }
  assert_eq!(ran.deletes.load(Ordering::SeqCst), 0);
  // After the handshake: every method.
  assert_eq!(
    request(send, Method::DELETE, None).await,
    (StatusCode::OK, "deleted".into())
  );
  conn.close(0u32.into(), b"done");
  server.stop().await;
}

/// A mutation that arrives in early data never reaches its handler: `425 Too Early`
/// (RFC 8470). The client's retry after the handshake is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_rtt_mutation_gets_425() {
  let ran = Ran::default();
  let server = Server::start(
    app(&ran),
    Http3Options::new().dangerous_allow_zero_rtt_for_queries(),
  )
  .await;
  let relay = Relay::start(server.endpoint.local_addr()).await;
  let client = quinn::Endpoint::client(local()).unwrap();
  let (conn, send) = resumed_in_0rtt(&client, &server.early_client(), &relay).await;

  let mutation = spawn_request(&send, Method::POST, Some("{\"amount\":100}"));
  let query = spawn_request(&send, Method::GET, None);
  reaches(&ran.reads, 2).await;
  tokio::time::sleep(Duration::from_millis(200)).await;
  assert_eq!(server.endpoint.stats().early_handovers, 2);

  relay.release();
  let (status, body) = mutation.await.unwrap();
  assert_eq!(status, StatusCode::TOO_EARLY);
  assert!(body.contains("handshake"), "{body}");
  assert_eq!(query.await.unwrap(), (StatusCode::OK, "item".into()));
  assert_eq!(
    ran.writes.load(Ordering::SeqCst),
    0,
    "the handler never ran"
  );

  // The retry, after the handshake, is served.
  assert_eq!(
    request(send, Method::POST, Some("{\"amount\":100}")).await,
    (StatusCode::OK, "written".into())
  );
  assert_eq!(ran.writes.load(Ordering::SeqCst), 1);
  conn.close(0u32.into(), b"done");
  server.stop().await;
}

/// F36: `Http3Options::congestion(Cubic)` (here from the config key
/// `server.http3.congestion`) runs the `h3` route on CUBIC; the default stays BBR.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http3_congestion_switch_to_cubic() {
  // Dotted paths, as a profile file (`[server.http3] congestion = "cubic"`) provides them.
  let provider = MapConfigProvider::from_pairs_with_options(
    [(CONGESTION_KEY, "cubic")],
    ProviderOptions {
      path_delim: ".",
      ..ProviderOptions::default()
    },
  );
  let options = Http3Options::config().load(&provider).unwrap();
  assert_eq!(options.congestion_control(), CongestionControl::Cubic);
  assert_eq!(
    Http3Options::default().congestion_control(),
    CongestionControl::Bbr
  );
  for (options, expected) in [
    (options, CongestionControl::Cubic),
    (Http3Options::default(), CongestionControl::Bbr),
  ] {
    let ran = Ran::default();
    let server = Server::start(app(&ran), options).await;
    assert_eq!(
      server.endpoint.router().congestion_control(ALPN_H3),
      Some(expected)
    );
    let client = quinn::Endpoint::client(local()).unwrap();
    let conn = client
      .connect_with(
        server.early_client(),
        server.endpoint.local_addr(),
        "localhost",
      )
      .unwrap()
      .await
      .unwrap();
    let send = h3_client(conn.clone()).await;
    assert_eq!(
      request(send, Method::GET, None).await,
      (StatusCode::OK, "item".into())
    );
    let stats = server.endpoint.stats();
    let counted = match expected {
      CongestionControl::Cubic => (stats.congestion_cubic, stats.congestion_bbr),
      _ => (stats.congestion_bbr, stats.congestion_cubic),
    };
    assert_eq!(counted, (1, 0), "{expected}");
    conn.close(0u32.into(), b"done");
    server.stop().await;
  }
}

#[test]
fn the_relay_reads_coalesced_packets() {
  let initial = [0xc0, 0, 0, 0, 1, 1, 0xaa, 1, 0xbb, 0, 1, 0x00];
  let handshake = [0xe0, 0, 0, 0, 1, 0, 0, 1, 0x00];
  assert!(!carries_handshake(&initial));
  assert!(carries_handshake(&[&initial[..], &handshake[..]].concat()));
  assert!(carries_handshake(&[0x40, 1, 2]));
  assert!(!carries_handshake(&[0xf0, 0, 0, 0, 1, 0, 0, 9, 9]));
  assert!(!carries_handshake(&[0xc0, 0, 0]));
  assert!(!carries_handshake(&[0xc0, 0, 0, 0, 1, 0]));
  assert!(!carries_handshake(&[0xc0, 0, 0, 0, 1, 0, 0, 0x40]));
}
