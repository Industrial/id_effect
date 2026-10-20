//! TLS 1.3 with mutual authentication on the TCP transport (feature `tls-tcp`, dist.tls #2 #4,
//! ADR 0013 R8): CA-issued PEM certificates in both directions, cookie sessions bound to the TLS
//! channel, untrusted peers refused, rotation for new connections, and the opt-outs (including
//! plaintext TCP) recorded in the posture registry.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_node::handshake::Cookie;
use id_effect_node::id::NodeId;
use id_effect_node::session::Session;
use id_effect_node::transport::certs::{CertReloader, NodeCertificate, ca_bundle_from_pem};
use id_effect_node::transport::tcp::TcpTransport;
use id_effect_node::transport::tls_tcp::{
  DANGEROUS_ACCEPT_ANY_SERVER_CERT, DANGEROUS_NO_CLIENT_AUTH, TlsTcpConfig, TlsTcpConfigError,
};
use id_effect_node::transport::{Connection, Listener, Transport, TransportError};
use id_effect_node::wire::Frame;

const WAIT: Duration = Duration::from_secs(5);

/// A throwaway certificate authority.
struct Ca {
  cert: rcgen::Certificate,
  key: rcgen::KeyPair,
}

/// A node certificate issued by a [`Ca`], as the PEM files an operator ships.
struct Leaf {
  chain_pem: String,
  key_pem: String,
}

impl Ca {
  fn new(name: &str) -> Self {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, name);
    params.key_usages = vec![
      rcgen::KeyUsagePurpose::KeyCertSign,
      rcgen::KeyUsagePurpose::CrlSign,
      rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().expect("ca key");
    let cert = params.self_signed(&key).expect("ca cert");
    Self { cert, key }
  }

  fn roots(&self) -> rustls::RootCertStore {
    ca_bundle_from_pem(self.cert.pem().as_bytes()).expect("ca bundle")
  }

  /// A leaf for the default server name `"cluster"`, valid for server and client auth.
  fn issue(&self) -> Leaf {
    let mut params = rcgen::CertificateParams::new(vec!["cluster".to_string()]).expect("params");
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, "mesh node");
    params.extended_key_usages = vec![
      rcgen::ExtendedKeyUsagePurpose::ServerAuth,
      rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = params
      .signed_by(&key, &self.cert, &self.key)
      .expect("leaf cert");
    Leaf {
      chain_pem: format!("{}{}", cert.pem(), self.cert.pem()),
      key_pem: key.serialize_pem(),
    }
  }
}

/// A node presenting `leaf` and verifying peers against `trust`.
fn node(leaf: &Leaf, trust: &Ca) -> TlsTcpConfig {
  let cert =
    NodeCertificate::from_pem(leaf.chain_pem.as_bytes(), leaf.key_pem.as_bytes()).expect("pem");
  TlsTcpConfig::from_certificate(&cert)
    .expect("usable key")
    .with_client_verification(trust.roots())
}

fn node_id(name: &str, incarnation: u128) -> NodeId {
  NodeId::from_parts(name, incarnation)
}

/// A fresh directory under the system temp dir.
fn temp_dir(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("clock")
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "id-effect-tcp-tls-{tag}-{}-{nanos}",
    std::process::id()
  ));
  std::fs::create_dir_all(&dir).expect("temp dir");
  dir
}

/// Replace `path` atomically (write a sibling, then rename).
fn write_atomic(path: &Path, contents: &str) {
  let tmp = path.with_extension("tmp");
  std::fs::write(&tmp, contents).expect("write");
  std::fs::rename(&tmp, path).expect("rename");
}

async fn listen(config: TlsTcpConfig) -> Box<dyn Listener> {
  TcpTransport::tls(config)
    .listen("127.0.0.1:0")
    .await
    .expect("bind")
}

/// Dial and accept one connection; the accept result is the listener's verdict on the dialer.
async fn dial(
  listener: &dyn Listener,
  client: TlsTcpConfig,
) -> (
  Result<Box<dyn Connection>, TransportError>,
  Result<Box<dyn Connection>, TransportError>,
) {
  let addr = listener.local_addr();
  let transport = TcpTransport::tls(client);
  tokio::time::timeout(WAIT, async {
    tokio::join!(transport.connect(&addr), listener.accept())
  })
  .await
  .expect("handshake finished")
}

#[tokio::test]
async fn tcp_tls_mutual_auth_round_trip() {
  let ca = Ca::new("cluster ca");
  let (a, b) = (node(&ca.issue(), &ca), node(&ca.issue(), &ca));
  let transport = TcpTransport::try_tls(a).expect("server trust");
  assert!(transport.is_encrypted());
  let listener = listen(b).await;
  let addr = listener.local_addr();
  let (dialed, accepted) = tokio::join!(transport.connect(&addr), listener.accept());
  let (dialed, accepted) = (dialed.expect("dial"), accepted.expect("accept"));

  // Both ends export the same TLS channel binding, which the cookie proof covers.
  let binding = dialed.channel_binding().expect("dialer binding");
  assert_eq!(accepted.channel_binding(), Some(binding));

  let cookie = Cookie::try_new("a-cluster-cookie-of-at-least-32-bytes").expect("cookie");
  let (id_a, id_b) = (node_id("a@tls", 1), node_id("b@tls", 2));
  let (to_b, to_a) = tokio::join!(
    Session::initiate(dialed, &id_a, &cookie),
    Session::respond(accepted, &id_b, &cookie),
  );
  let (to_b, to_a) = (to_b.expect("initiator"), to_a.expect("responder"));
  assert_eq!(to_b.peer(), &id_b);
  assert_eq!(to_a.peer(), &id_a);

  let big = Frame::Data(vec![7; 256 * 1024]);
  let (sent, got) = tokio::join!(to_b.send(&big), to_a.recv());
  sent.expect("send");
  assert_eq!(got.expect("recv"), big);
  to_a.send(&Frame::Ping(9)).await.expect("send back");
  assert_eq!(to_b.recv().await.expect("recv back"), Frame::Ping(9));
}

#[tokio::test]
async fn tcp_tls_rejects_untrusted_peer() {
  let (ca, other) = (Ca::new("cluster ca"), Ca::new("other ca"));
  let listener = listen(node(&ca.issue(), &ca)).await;

  // A client certificate from another CA: the listener refuses the handshake, and the dialer's
  // connection dies at its first read.
  let (dialed, accepted) = dial(listener.as_ref(), node(&other.issue(), &ca)).await;
  assert!(
    matches!(&accepted, Err(TransportError::Connect(m)) if m.contains("tls-tcp")),
    "{:?}",
    accepted.as_ref().err()
  );
  if let Ok(conn) = dialed {
    let _ = conn.send(Bytes::from_static(b"hello?")).await;
    assert!(
      tokio::time::timeout(WAIT, conn.recv())
        .await
        .expect("refusal observed")
        .is_err()
    );
  }

  // A server certificate from another CA: the dialer refuses it.
  let impostor = listen(node(&other.issue(), &other)).await;
  let (dialed, _) = dial(impostor.as_ref(), node(&ca.issue(), &ca)).await;
  assert!(
    matches!(&dialed, Err(TransportError::Connect(m)) if m.contains("certificate")),
    "{:?}",
    dialed.as_ref().err()
  );

  // A config with no trust neither dials nor listens.
  let bare = || {
    let leaf = ca.issue();
    let cert =
      NodeCertificate::from_pem(leaf.chain_pem.as_bytes(), leaf.key_pem.as_bytes()).expect("pem");
    TlsTcpConfig::from_certificate(&cert).expect("usable key")
  };
  assert_eq!(
    TcpTransport::try_tls(bare()).err(),
    Some(TlsTcpConfigError::NoServerTrust)
  );
  assert!(matches!(
    TcpTransport::tls(bare()).listen("127.0.0.1:0").await.err(),
    Some(TransportError::Bind(m)) if m.contains("no client trust")
  ));

  // The listener keeps serving trusted peers after refusing others.
  let (dialed, accepted) = dial(listener.as_ref(), node(&ca.issue(), &ca)).await;
  let (dialed, accepted) = (dialed.expect("dial"), accepted.expect("accept"));
  dialed.send(Bytes::from_static(b"ok")).await.expect("send");
  assert_eq!(
    accepted.recv().await.expect("recv"),
    Bytes::from_static(b"ok")
  );
}

#[tokio::test]
async fn tcp_tls_rotated_cert_used_for_new_connections() {
  let ca = Ca::new("cluster ca");
  let dir = temp_dir("rotate");
  let (chain, key) = (dir.join("node.crt"), dir.join("node.key"));
  let first = ca.issue();
  write_atomic(&chain, &first.chain_pem);
  write_atomic(&key, &first.key_pem);
  let reloader = CertReloader::new(&chain, &key).expect("load");
  let before = reloader.cert_sha256();
  let server = TlsTcpConfig::from_reloader(&reloader).with_client_verification(ca.roots());
  let listener = listen(server).await;

  let pinning = |pin| {
    let leaf = ca.issue();
    let cert =
      NodeCertificate::from_pem(leaf.chain_pem.as_bytes(), leaf.key_pem.as_bytes()).expect("pem");
    TlsTcpConfig::from_certificate(&cert)
      .expect("usable key")
      .with_client_verification(ca.roots())
      .with_pinned_server_cert_sha256(pin)
  };
  let (dialed, accepted) = dial(listener.as_ref(), pinning(before)).await;
  let (old_conn, _old_accepted) = (dialed.expect("old cert"), accepted.expect("accept"));

  let second = ca.issue();
  write_atomic(&chain, &second.chain_pem);
  write_atomic(&key, &second.key_pem);
  assert!(reloader.reload_if_changed().expect("reload"));
  let after = reloader.cert_sha256();
  assert_ne!(before, after);

  let (dialed, _) = dial(listener.as_ref(), pinning(after)).await;
  dialed.expect("new connections present the rotated certificate");
  let (dialed, _) = dial(listener.as_ref(), pinning(before)).await;
  assert!(
    dialed.is_err(),
    "the old certificate is no longer presented"
  );
  assert!(
    old_conn.channel_binding().is_some(),
    "open connections stay up"
  );
  let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn tcp_tls_opt_outs_are_recorded_in_posture() {
  let a = TlsTcpConfig::self_signed("node-a")
    .expect("cert")
    .dangerous_accept_any_server_cert();
  let b = TlsTcpConfig::self_signed("node-b")
    .expect("cert")
    .dangerous_no_client_auth();
  let listener = listen(b).await;
  let (dialed, accepted) = dial(listener.as_ref(), a).await;
  dialed.expect("dial with any server cert");
  accepted.expect("accept without client auth");
  let recorded = dangerous_opt_outs();
  for id in [DANGEROUS_ACCEPT_ANY_SERVER_CERT, DANGEROUS_NO_CLIENT_AUTH] {
    assert!(recorded.iter().any(|(r, _)| *r == id), "{id} not recorded");
  }
}

#[cfg(feature = "insecure-plaintext-tcp")]
#[tokio::test]
async fn plaintext_tcp_opt_out_is_recorded() {
  use id_effect_node::transport::tcp::DANGEROUS_PLAINTEXT;

  let plain = TcpTransport::dangerous_plaintext();
  assert!(!plain.is_encrypted());
  let recorded = dangerous_opt_outs();
  assert!(
    recorded
      .iter()
      .any(|(id, reopens)| *id == DANGEROUS_PLAINTEXT && reopens.contains("unencrypted")),
    "{recorded:?}"
  );
  let listener = plain.listen("127.0.0.1:0").await.expect("bind");
  let addr = listener.local_addr();
  let (dialed, accepted) = tokio::join!(plain.connect(&addr), listener.accept());
  let (dialed, accepted) = (dialed.expect("dial"), accepted.expect("accept"));
  assert_eq!(dialed.channel_binding(), None, "no channel to bind");
  dialed
    .send(Bytes::from_static(b"clear"))
    .await
    .expect("send");
  assert_eq!(
    accepted.recv().await.expect("recv"),
    Bytes::from_static(b"clear")
  );
}
