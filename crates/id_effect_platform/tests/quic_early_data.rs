//! 0-RTT early data per route (F25, ADR 0013 R10): off by default; a route opts in with the
//! recorded `Route::dangerous_allow_early_data` and then receives connections before their
//! handshake completes; the mesh (`idm/1`) never takes early data.
//!
//! A UDP relay between client and server holds back the server's handshake flight (its
//! Handshake and 1-RTT packets) on demand: the client keeps sending 0-RTT and cannot finish
//! the handshake, so "before the handshake completes" is a state the tests control instead of
//! a race on loopback.
#![cfg(feature = "quic")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_platform::quic::{
  ALPN_H3, ALPN_MESH, AlpnRoute, DANGEROUS_ALLOW_EARLY_DATA, EndpointStats, QuicEndpoint,
  QuicEndpointConfig, QuicError, QuicLimits, Refusal, Route, transport_config,
};
use id_effect_platform::tls::{Identity, NodeCertificate, client_tls_config};
use tokio::net::UdpSocket;
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(10);

fn local() -> SocketAddr {
  "127.0.0.1:0".parse().expect("addr")
}

/// The server's certificate, also the client certificate the mesh pins.
struct Peer {
  cert: NodeCertificate,
  identity: Identity,
}

impl Peer {
  fn new() -> Self {
    let cert = NodeCertificate::self_signed("localhost").expect("cert");
    let identity = Identity::from_certificate(&cert).expect("identity");
    Self { cert, identity }
  }

  fn endpoint(&self, limits: QuicLimits) -> QuicEndpoint {
    QuicEndpoint::bind(
      local(),
      QuicEndpointConfig::new(self.identity.clone()).with_limits(limits),
    )
    .expect("bind")
  }

  /// A client config offering `alpn` and asking for early data; its session cache (shared by
  /// clones) keeps the server's tickets for the next dial.
  fn early_client(&self, alpn: &[u8]) -> quinn::ClientConfig {
    let mut tls = client_tls_config(&self.identity, None, &[self.cert.cert_sha256()]).expect("tls");
    tls.enable_early_data = true;
    tls.alpn_protocols = vec![alpn.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic tls");
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(
      transport_config(&QuicLimits::default()).expect("transport"),
    ));
    config
  }
}

// --- a relay that can hold the server's handshake flight ----------------------------------------

/// Relays one client's datagrams to `server` and back; while held, server datagrams carrying a
/// Handshake or 1-RTT packet (its `Finished`, and everything after it) wait, so the client
/// cannot complete the handshake and keeps sending 0-RTT.
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
    let socket = UdpSocket::bind(local()).await.expect("relay");
    let addr = socket.local_addr().expect("relay addr");
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

  /// From now on, hold the server's handshake flight.
  fn hold(&self) {
    self.hold.store(true, Ordering::SeqCst);
  }

  /// Send what was held and relay everything again.
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

/// Whether a datagram carries a Handshake or a 1-RTT packet: the coalesced long-header
/// packets are walked (RFC 9000 §12.2, §17.2); a Retry carries neither.
fn carries_handshake(mut datagram: &[u8]) -> bool {
  while let Some(&first) = datagram.first() {
    if first & 0x80 == 0 {
      return true; // short header: 1-RTT
    }
    match (first >> 4) & 0x03 {
      0b10 => return true,  // Handshake
      0b11 => return false, // Retry
      kind => match skip_long_header(datagram, kind) {
        Some(rest) => datagram = rest,
        None => return false,
      },
    }
  }
  false
}

/// The bytes after the long-header packet that starts `datagram` (of type `kind`).
fn skip_long_header(datagram: &[u8], kind: u8) -> Option<&[u8]> {
  let mut at = 5; // first byte, version
  let dcid = usize::from(*datagram.get(at)?);
  at += 1 + dcid;
  let scid = usize::from(*datagram.get(at)?);
  at += 1 + scid;
  if kind == 0b00 {
    let (token, n) = varint(datagram.get(at..)?)?;
    at += n + token;
  }
  let (length, n) = varint(datagram.get(at..)?)?;
  datagram.get(at + n + length..)
}

// --- helpers ---------------------------------------------------------------------------------

/// One bidirectional exchange: `ping` out, the reply back.
async fn exchange(conn: &quinn::Connection, ping: &[u8]) -> Vec<u8> {
  let (mut send, mut recv) = conn.open_bi().await.expect("open");
  send.write_all(ping).await.expect("write");
  send.finish().expect("finish");
  tokio::time::timeout(WAIT, recv.read_to_end(1024))
    .await
    .expect("reply in time")
    .expect("reply")
}

/// Answer every stream of `conn` with `pong`.
fn echo(conn: quinn::Connection) {
  tokio::spawn(async move {
    while let Ok((mut send, mut recv)) = conn.accept_bi().await {
      let _ = recv.read_to_end(1024).await;
      let _ = send.write_all(b"pong").await;
      let _ = send.finish();
    }
  });
}

/// A first, full handshake through `relay`: the client keeps the ticket for the next dial.
async fn first_dial(client: &quinn::Endpoint, config: &quinn::ClientConfig, relay: &Relay) {
  let conn = client
    .connect_with(config.clone(), relay.addr, "localhost")
    .expect("connect")
    .await
    .expect("first handshake");
  assert_eq!(exchange(&conn, b"ping").await, b"pong");
  // The ticket rides with the server's first 1-RTT flight; the exchange waited for it.
  tokio::time::sleep(Duration::from_millis(50)).await;
  conn.close(0u32.into(), b"done");
}

async fn next_early(
  route: &AlpnRoute,
) -> Result<(quinn::Connection, id_effect_platform::quic::Handshake), Refusal> {
  tokio::time::timeout(WAIT, route.accept_early())
    .await
    .expect("route saw the attempt")
    .expect("endpoint running")
}

async fn until(endpoint: &QuicEndpoint, done: impl Fn(&EndpointStats) -> bool) -> EndpointStats {
  tokio::time::timeout(WAIT, async {
    loop {
      let stats = endpoint.stats();
      if done(&stats) {
        return stats;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("counter reached")
}

// --- tests -----------------------------------------------------------------------------------

/// Off by default: the tickets of an endpoint whose routes take no early data allow none, so a
/// resumed client cannot send 0-RTT; its connections are delivered after the handshake.
#[tokio::test]
async fn zero_rtt_off_by_default() {
  let peer = Peer::new();
  let endpoint = peer.endpoint(QuicLimits::default());
  assert!(!Route::h3().early_data());
  let h3 = endpoint.register(Route::h3()).expect("h3");
  let router = endpoint.router();
  assert_eq!(router.early_data(ALPN_H3), Some(false));
  assert_eq!(router.early_data(b"other"), None);
  let relay = Relay::start(endpoint.local_addr()).await;
  let client = quinn::Endpoint::client(local()).expect("client");
  let config = peer.early_client(ALPN_H3);
  let server = tokio::spawn(async move {
    let (conn, handshake) = next_early(&h3).await.expect("delivered");
    assert!(handshake.is_complete(), "delivered after the handshake");
    assert!(handshake.completed().await);
    echo(conn);
    h3
  });
  first_dial(&client, &config, &relay).await;
  let _h3 = server.await.expect("server");
  let second = client
    .connect_with(config, relay.addr, "localhost")
    .expect("connect");
  assert!(second.into_0rtt().is_err(), "the server offers no 0-RTT");
  assert_eq!(endpoint.stats().early_handovers, 0);
}

/// The opt-out: an `h3` route with early data receives a resumed client's connection before
/// the handshake completes and reads its 0-RTT request; the handshake then completes.
#[tokio::test]
async fn h3_route_with_early_data_reads_0rtt_before_the_handshake() {
  let peer = Peer::new();
  let endpoint = peer.endpoint(QuicLimits::default());
  let route = Route::h3().dangerous_allow_early_data();
  assert!(route.early_data());
  assert!(
    dangerous_opt_outs()
      .iter()
      .any(|(id, _)| *id == DANGEROUS_ALLOW_EARLY_DATA)
  );
  let h3 = endpoint.register(route).expect("h3");
  assert_eq!(endpoint.router().early_data(ALPN_H3), Some(true));
  let relay = Relay::start(endpoint.local_addr()).await;
  let client = quinn::Endpoint::client(local()).expect("client");
  let config = peer.early_client(ALPN_H3);

  let server = tokio::spawn(async move {
    // The first connection (a full handshake) is served like any other.
    let (first, _) = next_early(&h3).await.expect("first");
    echo(first);
    let (conn, handshake) = next_early(&h3).await.expect("second");
    let early_at_accept = !handshake.is_complete();
    let (mut send, mut recv) = conn.accept_bi().await.expect("early stream");
    let request = recv.read_to_end(1024).await.expect("early request");
    let early_at_read = !handshake.is_complete();
    send.write_all(b"seen").await.expect("reply");
    send.finish().expect("finish");
    (early_at_accept, early_at_read, request, handshake, conn)
  });
  first_dial(&client, &config, &relay).await;

  relay.hold();
  let connecting = client
    .connect_with(config, relay.addr, "localhost")
    .expect("connect");
  let Ok((conn, accepted)) = connecting.into_0rtt() else {
    panic!("the ticket allows early data");
  };
  // Sent in 0-RTT; the server reads and answers it while its handshake flight is held back.
  let reply = {
    let conn = conn.clone();
    tokio::spawn(async move { exchange(&conn, b"early").await })
  };
  let (early_at_accept, early_at_read, request, handshake, _served) =
    tokio::time::timeout(WAIT, server)
      .await
      .expect("server in time")
      .expect("server");
  assert!(early_at_accept && early_at_read);
  assert_eq!(request, b"early");
  assert!(!handshake.is_complete());

  relay.release();
  assert!(
    tokio::time::timeout(WAIT, handshake.completed())
      .await
      .expect("handshake in time")
  );
  assert!(handshake.is_complete());
  assert_eq!(reply.await.expect("client"), b"seen");
  assert!(accepted.await, "the server accepted the 0-RTT data");
  let stats = until(&endpoint, |s| s.early_handovers == 2).await;
  assert_eq!(stats.refused_early_data, 0);
}

/// The mesh never takes early data: the opt-in is refused on `idm/1` and on any route that
/// requires a verified client, and a mesh client that sends 0-RTT on an endpoint whose `h3`
/// route takes early data is closed before the mesh sees anything of it.
#[tokio::test]
async fn mesh_route_never_accepts_early_data() {
  let peer = Peer::new();
  let endpoint = peer.endpoint(QuicLimits::default());
  let pins = vec![peer.cert.cert_sha256()];
  for route in [
    Route::mesh(None, pins.clone()),
    Route::new(b"custom".to_vec()).require_verified_client(None, pins.clone()),
  ] {
    let refused = endpoint.register(route.dangerous_allow_early_data());
    assert!(
      matches!(&refused, Err(QuicError::Config(m)) if m.contains("cannot take early data")),
      "{refused:?}"
    );
  }
  assert!(endpoint.router().alpns().is_empty());

  let _h3 = endpoint
    .register(Route::h3().dangerous_allow_early_data())
    .expect("h3");
  let mesh = endpoint.register(Route::mesh(None, pins)).expect("mesh");
  assert_eq!(endpoint.router().early_data(ALPN_MESH), Some(false));
  let relay = Relay::start(endpoint.local_addr()).await;
  let client = quinn::Endpoint::client(local()).expect("client");
  let config = peer.early_client(ALPN_MESH);

  let server = tokio::spawn(async move {
    let (first, handshake) = next_early(&mesh).await.expect("first mesh connection");
    assert!(
      handshake.is_complete(),
      "the mesh gets connections after the handshake"
    );
    echo(first);
    next_early(&mesh).await
  });
  first_dial(&client, &config, &relay).await;

  relay.hold();
  let connecting = client
    .connect_with(config, relay.addr, "localhost")
    .expect("connect");
  // The endpoint's tickets allow early data (its h3 route takes it), so the client tries.
  let Ok((conn, _accepted)) = connecting.into_0rtt() else {
    panic!("the ticket allows early data");
  };
  let (mut send, _recv) = conn.open_bi().await.expect("open");
  send.write_all(b"mesh frame").await.expect("write");
  let _ = send.finish();

  let refusal = tokio::time::timeout(WAIT, server)
    .await
    .expect("server in time")
    .expect("server");
  assert!(
    matches!(refusal, Err(Refusal::EarlyData { .. })),
    "{refusal:?}"
  );
  assert!(
    refusal
      .unwrap_err()
      .to_string()
      .contains("early data (0-RTT)")
  );
  let stats = until(&endpoint, |s| s.refused_early_data == 1).await;
  assert_eq!(stats.early_handovers, 0);
  relay.release();
  tokio::time::timeout(WAIT, conn.closed())
    .await
    .expect("the client sees the close");
}

/// An early connection whose handshake never completes is closed at the handshake timeout;
/// `AlpnRoute::accept` (which waits for handshakes) skips it and reports the refusal.
#[tokio::test]
async fn early_connection_without_a_completed_handshake_times_out() {
  let peer = Peer::new();
  let mut limits = QuicLimits::default();
  limits.handshake_timeout = Duration::from_millis(500);
  let endpoint = peer.endpoint(limits);
  let h3 = endpoint
    .register(Route::h3().dangerous_allow_early_data())
    .expect("h3");
  let relay = Relay::start(endpoint.local_addr()).await;
  let client = quinn::Endpoint::client(local()).expect("client");
  let config = peer.early_client(ALPN_H3);

  let server = tokio::spawn(async move {
    let first = tokio::time::timeout(WAIT, h3.accept())
      .await
      .expect("in time")
      .expect("running")
      .expect("first");
    echo(first);
    tokio::time::timeout(WAIT, h3.accept())
      .await
      .expect("in time")
      .expect("running")
  });
  first_dial(&client, &config, &relay).await;

  relay.hold();
  let connecting = client
    .connect_with(config, relay.addr, "localhost")
    .expect("connect");
  let Ok((conn, _accepted)) = connecting.into_0rtt() else {
    panic!("the ticket allows early data");
  };
  let (mut send, _recv) = conn.open_bi().await.expect("open");
  send.write_all(b"never confirmed").await.expect("write");
  let _ = send.finish();

  let refused = tokio::time::timeout(WAIT, server)
    .await
    .expect("server in time")
    .expect("server");
  assert!(
    matches!(&refused, Err(Refusal::Handshake { reason, .. }) if reason.contains("timed out")),
    "{refused:?}"
  );
  let stats = until(&endpoint, |s| s.handshakes_failed == 1).await;
  assert_eq!(stats.early_handovers, 2);
}

/// An early route whose receiver is gone closes the connection at once (as any route).
#[tokio::test]
async fn early_route_without_a_receiver_closes_its_connections() {
  let peer = Peer::new();
  let endpoint = peer.endpoint(QuicLimits::default());
  let h3 = endpoint
    .register(Route::h3().dangerous_allow_early_data())
    .expect("h3");
  drop(h3);
  let client = quinn::Endpoint::client(local()).expect("client");
  let config = peer.early_client(ALPN_H3);
  let connecting = client
    .connect_with(config, endpoint.local_addr(), "localhost")
    .expect("connect");
  if let Ok(Ok(conn)) = tokio::time::timeout(WAIT, connecting).await {
    tokio::time::timeout(WAIT, conn.closed())
      .await
      .expect("closed");
  }
  let stats = until(&endpoint, |s| s.refused_no_route == 1).await;
  assert_eq!(stats.early_handovers, 0);
}

#[test]
fn the_relay_reads_coalesced_packets() {
  // Initial (empty token, 1-byte payload) coalesced with a Handshake packet.
  let initial = [0xc0, 0, 0, 0, 1, 1, 0xaa, 1, 0xbb, 0, 1, 0x00];
  let handshake = [0xe0, 0, 0, 0, 1, 0, 0, 1, 0x00];
  assert!(!carries_handshake(&initial));
  assert!(carries_handshake(&[&initial[..], &handshake[..]].concat()));
  // 0-RTT only; a short header; a Retry.
  let zero_rtt = [0xd0, 0, 0, 0, 1, 0, 0, 0x40, 0x01, 0x00];
  assert!(!carries_handshake(&zero_rtt));
  assert!(carries_handshake(&[0x40, 1, 2]));
  assert!(!carries_handshake(&[0xf0, 0, 0, 0, 1, 0, 0, 9, 9]));
  // Truncated headers are not held.
  assert!(!carries_handshake(&[0xc0, 0, 0]));
  assert_eq!(varint(&[0x40, 0x10]), Some((16, 2)));
  assert_eq!(varint(&[]), None);
}
