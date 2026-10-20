//! 0.10.1 security regressions for the mesh transports and handshake:
//! server trust on QUIC (H3), proofs bound to the channel and transcript
//! (H4), and pre-authentication bounds (M9).

use std::time::Duration;

use id_effect_node::handshake::{Cookie, CookieError, HandshakeError, MIN_COOKIE_BYTES};
use id_effect_node::id::NodeId;
use id_effect_node::session::{Session, SessionError};
use id_effect_node::transport::Transport;
#[cfg(any(feature = "quic", feature = "insecure-plaintext-tcp"))]
use id_effect_node::transport::TransportError;
use id_effect_node::transport::memory::MemoryNetwork;
#[cfg(feature = "insecure-plaintext-tcp")]
use id_effect_node::transport::tcp::TcpTransport;
use id_effect_node::wire::{Frame, HandshakeMsg, PROTOCOL_VERSION, decode, encode};

const SECRET: &str = "a-cluster-cookie-of-at-least-32-bytes";

fn cookie() -> Cookie {
  Cookie::try_new(SECRET).expect("long enough")
}

fn id(name: &str, incarnation: u128) -> NodeId {
  NodeId::from_parts(name, incarnation)
}

/// The v2 handshake codec as seen on the wire (same layout as
/// `wire::HandshakeMsg`, 128-bit nonces), so the test MITM can rewrite one.
#[derive(serde::Serialize, serde::Deserialize)]
enum V2Frame {
  Handshake(V2Msg),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[allow(dead_code)] // mirrors the full layout; the test inspects two variants
enum V2Msg {
  Hello {
    version: u16,
    node: NodeId,
  },
  Challenge {
    version: u16,
    node: NodeId,
    nonce: u128,
  },
  ChallengeReply {
    digest: [u8; 32],
    nonce: u128,
  },
  Welcome {
    digest: [u8; 32],
  },
  Rejected {
    reason: String,
  },
}

#[cfg(feature = "quic")]
mod quic {
  use std::sync::Arc;

  use id_effect_node::transport::Connection;
  use id_effect_node::transport::quic::{QuicConfig, QuicConfigError, QuicTransport};

  use super::*;

  /// Forward every frame from `from` to `to` until either side closes.
  fn pump(from: Arc<dyn Connection>, to: Arc<dyn Connection>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
      while let Ok(frame) = from.recv().await {
        if to.send(frame).await.is_err() {
          break;
        }
      }
      to.close().await;
    })
  }

  #[tokio::test]
  async fn h3_quic_without_roots_is_a_config_error() {
    let bare = || QuicConfig::self_signed("node-a").expect("cert");
    assert_eq!(
      QuicTransport::try_new(bare()).err(),
      Some(QuicConfigError::NoServerTrust)
    );
    // The infallible constructor cannot silently fall back to accept-any.
    let err = QuicTransport::new(bare())
      .connect("127.0.0.1:9")
      .await
      .err()
      .expect("no trust, no dial");
    assert!(
      matches!(&err, TransportError::Connect(m) if m.contains("no server trust")),
      "{err}"
    );
    // Roots or a pin are each enough.
    let other = QuicConfig::self_signed("node-b").expect("cert");
    assert!(
      QuicTransport::try_new(bare().with_client_verification(rustls::RootCertStore::empty()))
        .is_ok()
    );
    assert!(
      QuicTransport::try_new(bare().with_pinned_server_cert_sha256(other.cert_sha256())).is_ok()
    );
  }

  #[tokio::test]
  async fn h3_dangerous_accept_any_is_explicit() {
    let client = QuicConfig::self_signed("node-a")
      .expect("cert")
      .dangerous_accept_any_server_cert();
    // 0.11 mutual TLS: the listener pins the dialer's certificate.
    let server = QuicConfig::self_signed("node-b")
      .expect("cert")
      .with_pinned_client_cert_sha256(client.cert_sha256());
    let listener = QuicTransport::new(server)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr();
    let client = QuicTransport::try_new(client).expect("opted out");
    let (dialed, accepted) = tokio::join!(
      async {
        let conn = client.connect(&addr).await.expect("dial");
        conn
          .send(bytes::Bytes::from_static(b"hi"))
          .await
          .expect("send");
        conn
      },
      async { listener.accept().await.expect("accept") }
    );
    assert_eq!(
      accepted.recv().await.expect("recv"),
      bytes::Bytes::from_static(b"hi")
    );
    drop(dialed);
  }

  /// A man-in-the-middle terminates TLS with both nodes (the lured
  /// initiator accepts its certificate) and relays every handshake frame
  /// unchanged. The proofs are bound to each TLS channel's exporter
  /// secret, so the responder refuses the relayed proof.
  #[tokio::test]
  async fn h4_relayed_proof_fails() {
    let (a, b) = (id("a@test", 1), id("b@test", 2));

    let a_config = QuicConfig::self_signed("node-a").expect("cert");
    let m_client_config = QuicConfig::self_signed("mitm-client").expect("cert");
    // 0.11 mutual TLS: B has also been lured into trusting M's client
    // certificate, so only the cookie's channel binding stands in the way.
    let b_config = QuicConfig::self_signed("node-b")
      .expect("cert")
      .with_pinned_client_cert_sha256(m_client_config.cert_sha256());
    let b_pin = b_config.cert_sha256();
    let b_listener = QuicTransport::new(b_config)
      .listen("127.0.0.1:0")
      .await
      .expect("bind b");
    let b_addr = b_listener.local_addr();

    let m_config = QuicConfig::self_signed("mitm")
      .expect("cert")
      .with_pinned_client_cert_sha256(a_config.cert_sha256());
    let m_pin = m_config.cert_sha256();
    let m_listener = QuicTransport::new(m_config)
      .listen("127.0.0.1:0")
      .await
      .expect("bind m");
    let m_addr = m_listener.local_addr();
    // M trusts B properly; A has been lured into pinning M.
    let m_dialer = QuicTransport::try_new(m_client_config.with_pinned_server_cert_sha256(b_pin))
      .expect("m dialer");
    let a_dialer =
      QuicTransport::try_new(a_config.with_pinned_server_cert_sha256(m_pin)).expect("a dialer");

    let b_side = {
      let b = b.clone();
      tokio::spawn(async move {
        let conn = b_listener.accept().await.expect("b accept");
        Session::respond(conn, &b, &cookie())
          .await
          .map(|s| s.peer().clone())
      })
    };
    let relay = tokio::spawn(async move {
      let from_a: Arc<dyn Connection> = Arc::from(m_listener.accept().await.expect("m accept"));
      let to_b: Arc<dyn Connection> = Arc::from(m_dialer.connect(&b_addr).await.expect("m dial"));
      let up = pump(Arc::clone(&from_a), Arc::clone(&to_b));
      let down = pump(to_b, from_a);
      let _ = tokio::join!(up, down);
    });

    let a_conn = a_dialer.connect(&m_addr).await.expect("a dial m");
    let a_result = Session::initiate(a_conn, &a, &cookie()).await;
    let b_result = b_side.await.expect("join b");
    relay.abort();

    assert!(
      matches!(
        b_result,
        Err(SessionError::Handshake(HandshakeError::BadCookie))
      ),
      "responder accepted a relayed proof: {b_result:?}"
    );
    assert!(a_result.is_err(), "initiator authenticated through a relay");
  }

  /// Without a relay, the same handshake over QUIC succeeds.
  #[tokio::test]
  async fn h4_direct_quic_handshake_succeeds() {
    let (a, b) = (id("a@test", 1), id("b@test", 2));
    let a_config = QuicConfig::self_signed("node-a").expect("cert");
    let b_config = QuicConfig::self_signed("node-b")
      .expect("cert")
      .with_pinned_peer_cert_sha256(a_config.cert_sha256());
    let b_pin = b_config.cert_sha256();
    let listener = QuicTransport::new(b_config)
      .listen("127.0.0.1:0")
      .await
      .expect("bind");
    let addr = listener.local_addr();
    let dialer =
      QuicTransport::try_new(a_config.with_pinned_peer_cert_sha256(b_pin)).expect("dialer");
    let responder = {
      let b = b.clone();
      tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        let session = Session::respond(conn, &b, &cookie())
          .await
          .expect("respond");
        let frame = session.recv().await.expect("recv");
        (session.peer().clone(), frame)
      })
    };
    let conn = dialer.connect(&addr).await.expect("dial");
    let session = Session::initiate(conn, &a, &cookie())
      .await
      .expect("initiate");
    assert_eq!(session.peer(), &b);
    // The pre-auth cap is lifted once authenticated.
    let big = Frame::Data(vec![1; 64 * 1024]);
    session.send(&big).await.expect("send");
    let (peer, frame) = responder.await.expect("join");
    assert_eq!(peer, a);
    assert_eq!(frame, big);
  }
}

/// On a transport without a channel, a man-in-the-middle that presents
/// itself under its own name and relays the responder's challenge gets a
/// proof bound to its name, which the real responder refuses (v1 MACed only
/// the nonce and the prover's name, so this relay succeeded).
#[tokio::test]
async fn h4_relayed_proof_fails_without_a_channel() {
  let net = MemoryNetwork::new();
  let (a, b, m) = (id("a@test", 1), id("b@test", 2), id("m@evil", 3));
  let secret = cookie();

  // B listens; M listens for A; A dials M; M dials B.
  let b_listener = net.transport("b").listen("b").await.expect("bind b");
  let m_listener = net.transport("m").listen("m").await.expect("bind m");
  let a_conn = net.transport("a").connect("m").await.expect("a dial");
  let m_from_a = m_listener.accept().await.expect("m accept");
  let m_to_b = net.transport("m").connect("b").await.expect("m dial");
  let b_conn = b_listener.accept().await.expect("b accept");

  let mitm = async {
    // A's Hello goes to B as-is: M claims to be A towards B.
    let hello = m_from_a.recv().await.expect("hello");
    m_to_b.send(hello).await.expect("forward hello");
    // B's challenge goes to A re-labelled with M's identity.
    let challenge = m_to_b.recv().await.expect("challenge");
    let V2Frame::Handshake(V2Msg::Challenge { version, nonce, .. }) =
      postcard::from_bytes(&challenge).expect("decode")
    else {
      panic!("expected Challenge");
    };
    let relabelled = V2Frame::Handshake(V2Msg::Challenge {
      version,
      node: m.clone(),
      nonce,
    });
    m_from_a
      .send(postcard::to_allocvec(&relabelled).expect("encode").into())
      .await
      .expect("relabelled challenge");
    // A's proof, made for M, is relayed to B.
    let reply = m_from_a.recv().await.expect("reply");
    m_to_b.send(reply).await.expect("relay proof");
    let verdict = m_to_b.recv().await.expect("verdict");
    m_from_a.close().await;
    postcard::from_bytes::<V2Frame>(&verdict).expect("decode verdict")
  };
  let (verdict, a_result, b_result) = tokio::join!(
    mitm,
    Session::initiate(a_conn, &a, &secret),
    Session::respond(b_conn, &b, &secret),
  );
  assert!(
    matches!(verdict, V2Frame::Handshake(V2Msg::Rejected { reason }) if reason == "bad cookie")
  );
  assert!(a_result.is_err());
  assert!(
    matches!(
      b_result,
      Err(SessionError::Handshake(HandshakeError::BadCookie))
    ),
    "responder accepted a proof made for another peer: {:?}",
    b_result.map(|s| s.peer().clone())
  );
}

#[tokio::test]
async fn m9_handshake_times_out() {
  let net = MemoryNetwork::new();
  let listener = net.transport("b").listen("b").await.expect("bind");
  let silent = net.transport("a").connect("b").await.expect("dial");
  let conn = listener.accept().await.expect("accept");
  let quick = cookie().with_handshake_timeout(Duration::from_millis(100));
  let started = std::time::Instant::now();
  let err = Session::respond(conn, &id("b@test", 2), &quick)
    .await
    .err()
    .expect("a silent peer must time out");
  assert!(
    matches!(&err, SessionError::Handshake(HandshakeError::Protocol(m)) if m.contains("timed out")),
    "{err}"
  );
  assert!(started.elapsed() < Duration::from_secs(5));
  assert_eq!(cookie().handshake_timeout(), Duration::from_secs(5));
  drop(silent);
}

// Raw bytes onto the socket: the plaintext TCP opt-out.
#[cfg(feature = "insecure-plaintext-tcp")]
#[tokio::test]
async fn m9_oversized_pre_auth_frame_rejected() {
  use id_effect_node::handshake::PRE_AUTH_MAX_FRAME_BYTES;
  use tokio::io::AsyncWriteExt;

  let listener = TcpTransport::dangerous_plaintext()
    .listen("127.0.0.1:0")
    .await
    .expect("bind");
  let addr = listener.local_addr();
  let (raw, accepted) = tokio::join!(tokio::net::TcpStream::connect(&addr), listener.accept());
  let mut raw = raw.expect("raw dial");
  let accepted = accepted.expect("accept");
  // A length prefix just over the pre-auth cap (far under MAX_FRAME_BYTES).
  raw
    .write_all(&(PRE_AUTH_MAX_FRAME_BYTES + 1).to_be_bytes())
    .await
    .expect("prefix");
  let err = Session::respond(accepted, &id("b@test", 2), &cookie())
    .await
    .err()
    .expect("oversized pre-auth frame");
  assert!(
    matches!(
      &err,
      SessionError::Handshake(HandshakeError::Transport(TransportError::Io(m)))
        if m.contains("before authentication")
    ),
    "{err}"
  );
}

#[cfg(feature = "insecure-plaintext-tcp")]
#[tokio::test]
async fn m9_frame_cap_lifts_after_authentication() {
  let tcp = TcpTransport::dangerous_plaintext();
  let listener = tcp.listen("127.0.0.1:0").await.expect("bind");
  let addr = listener.local_addr();
  let (dial, accept) = tokio::join!(tcp.connect(&addr), listener.accept());
  let (a, b, cookie) = (id("a@test", 1), id("b@test", 2), cookie());
  let (to_b, to_a) = tokio::join!(
    Session::initiate(dial.expect("dial"), &a, &cookie),
    Session::respond(accept.expect("accept"), &b, &cookie),
  );
  let (to_b, to_a) = (to_b.expect("a"), to_a.expect("b"));
  let big = Frame::Data(vec![9; 64 * 1024]);
  let (sent, got) = tokio::join!(to_b.send(&big), to_a.recv());
  sent.expect("send");
  assert_eq!(got.expect("recv"), big);
}

#[tokio::test]
async fn m9_short_cookie_rejected() {
  assert_eq!(
    Cookie::try_new("too-short").err(),
    Some(CookieError::TooShort {
      len: 9,
      min: MIN_COOKIE_BYTES
    })
  );
  assert!(Cookie::try_new(Vec::<u8>::new()).is_err(), "empty cookie");

  let net = MemoryNetwork::new();
  let listener = net.transport("b").listen("b").await.expect("bind");
  let dial = net.transport("a").connect("b").await.expect("dial");
  let _accept = listener.accept().await.expect("accept");
  let err = Session::initiate(dial, &id("a@test", 1), &Cookie::new(""))
    .await
    .err()
    .expect("empty cookie handshake");
  assert!(
    matches!(&err, SessionError::Handshake(HandshakeError::Protocol(m)) if m.starts_with("config: cookie is 0 bytes")),
    "{err}"
  );

  // The named opt-out still works, for tests and throwaway clusters.
  let short = Cookie::new("short").dangerous_allow_short();
  let listener = net.transport("c").listen("c").await.expect("bind");
  let dial = net.transport("a").connect("c").await.expect("dial");
  let accept = listener.accept().await.expect("accept");
  let (a, c) = (id("a@test", 1), id("c@test", 3));
  let (to_c, to_a) = tokio::join!(
    Session::initiate(dial, &a, &short),
    Session::respond(accept, &c, &short),
  );
  assert!(to_c.is_ok() && to_a.is_ok());
}

/// The protocol version is bumped: a 0.10.0 (v1) Hello is refused with a
/// version mismatch, and the older peer is told why.
#[tokio::test]
async fn h4_v1_peers_are_refused_with_version_mismatch() {
  let net = MemoryNetwork::new();
  let listener = net.transport("b").listen("b").await.expect("bind");
  let old = net.transport("a").connect("b").await.expect("dial");
  let conn = listener.accept().await.expect("accept");
  old
    .send(
      encode(&Frame::Handshake(HandshakeMsg::Hello {
        version: 1,
        node: id("a@old", 1),
      }))
      .expect("encode"),
    )
    .await
    .expect("send");
  let err = Session::respond(conn, &id("b@test", 2), &cookie())
    .await
    .err()
    .expect("refused");
  assert!(matches!(
    err,
    SessionError::Handshake(HandshakeError::VersionMismatch { ours, theirs: 1 })
      if ours == PROTOCOL_VERSION
  ));
  let refusal = decode(&old.recv().await.expect("refusal")).expect("decode");
  assert_eq!(
    refusal,
    Frame::Handshake(HandshakeMsg::Rejected {
      reason: format!("version mismatch: have {PROTOCOL_VERSION}, got 1")
    })
  );
}
