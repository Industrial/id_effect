//! Mutual TLS on the QUIC transport (dist.tls #4 #5 #7, ADR 0013 R8): both
//! ends present and verify certificates, operator-issued PEM files load, a
//! rotated certificate reaches new connections, and the opt-outs are
//! recorded in the posture registry.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_node::transport::quic::{
  ALPN_MESH, DANGEROUS_ACCEPT_ANY_SERVER_CERT, DANGEROUS_NO_CLIENT_AUTH, QuicConfig, QuicTransport,
};
use id_effect_node::transport::{Transport, TransportError};
use id_effect_platform::quic::{ClientAuthMode, QuicEndpoint, QuicEndpointConfig, Route};
use id_effect_platform::tls::{
  CertError, CertReloader, Identity, NodeCertificate, ca_bundle_from_pem, load_ca_bundle,
};
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(5);

/// Where the listener runs (ADR 0014 D7): its own endpoint with handshake-level mutual TLS
/// (`quic.mesh_port`), or the endpoint shared with HTTP/3, where client certificates are
/// optional in the handshake and required by the `idm/1` route. Every test runs on both.
#[derive(Clone, Copy, Debug)]
enum Shape {
  Own,
  Shared,
}

const SHAPES: [Shape; 2] = [Shape::Own, Shape::Shared];

/// A throwaway certificate authority.
struct Ca {
  cert: rcgen::Certificate,
  key: rcgen::KeyPair,
}

/// A node certificate issued by a [`Ca`], as the PEM files an operator ships.
struct Leaf {
  chain_pem: String,
  key_pem: String,
  sha256: [u8; 32],
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

  fn bundle_pem(&self) -> String {
    self.cert.pem()
  }

  fn roots(&self) -> rustls::RootCertStore {
    ca_bundle_from_pem(self.bundle_pem().as_bytes()).expect("ca bundle")
  }

  /// A leaf for the default server name `"cluster"`, valid for server and
  /// client auth, with `key`; `key_pem` encodes the key as given.
  fn issue_with(&self, key: &rcgen::KeyPair, key_pem: String) -> Leaf {
    let mut params = rcgen::CertificateParams::new(vec!["cluster".to_string()]).expect("params");
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, "mesh node");
    params.extended_key_usages = vec![
      rcgen::ExtendedKeyUsagePurpose::ServerAuth,
      rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params
      .signed_by(key, &self.cert, &self.key)
      .expect("leaf cert");
    Leaf {
      chain_pem: format!("{}{}", cert.pem(), self.cert.pem()),
      key_pem,
      sha256: sha256(cert.der()),
    }
  }

  /// A P-256 leaf with a PKCS#8 key.
  fn issue(&self) -> Leaf {
    let key = rcgen::KeyPair::generate().expect("key");
    let pem = key.serialize_pem();
    self.issue_with(&key, pem)
  }
}

fn sha256(der: &[u8]) -> [u8; 32] {
  use sha2::Digest;
  sha2::Sha256::digest(der).into()
}

/// One DER TLV of `der`: (tag, contents, rest).
fn tlv(der: &[u8]) -> (u8, &[u8], &[u8]) {
  let (len, header) = if der[1] < 0x80 {
    (der[1] as usize, 2)
  } else {
    let n = (der[1] & 0x7f) as usize;
    let len = der[2..2 + n]
      .iter()
      .fold(0usize, |len, b| len << 8 | *b as usize);
    (len, 2 + n)
  };
  (der[0], &der[header..header + len], &der[header + len..])
}

/// The key inside a PKCS#8 `PrivateKeyInfo`: PKCS#1 for RSA, SEC1 for EC.
fn unwrap_pkcs8(pkcs8: &[u8]) -> Vec<u8> {
  let (_, info, _) = tlv(pkcs8);
  let (_, _, rest) = tlv(info); // version
  let (_, _, rest) = tlv(rest); // algorithm
  let (tag, key, _) = tlv(rest);
  assert_eq!(tag, 0x04, "privateKey OCTET STRING");
  key.to_vec()
}

fn pem(label: &str, der: &[u8]) -> String {
  const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut b64 = String::new();
  for chunk in der.chunks(3) {
    let bytes = [
      chunk[0],
      *chunk.get(1).unwrap_or(&0),
      *chunk.get(2).unwrap_or(&0),
    ];
    let n = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
    for i in 0..4 {
      if i <= chunk.len() {
        b64.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
      } else {
        b64.push('=');
      }
    }
  }
  let lines: Vec<&str> = b64
    .as_bytes()
    .chunks(64)
    .map(|line| std::str::from_utf8(line).expect("ascii"))
    .collect();
  format!(
    "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
    lines.join("\n")
  )
}

/// A fresh directory under the system temp dir.
fn temp_dir(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .expect("clock")
    .as_nanos();
  let dir = std::env::temp_dir().join(format!(
    "id-effect-tls-{tag}-{}-{nanos}",
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

/// A node presenting `leaf` and verifying peers against `trust`.
fn node(leaf: &Leaf, trust: &Ca) -> QuicConfig {
  let cert =
    NodeCertificate::from_pem(leaf.chain_pem.as_bytes(), leaf.key_pem.as_bytes()).expect("pem");
  QuicConfig::from_certificate(&cert)
    .expect("usable key")
    .with_client_verification(trust.roots())
}

/// Listen with `config` and echo one frame per connection. Each accept
/// result (`Err` = the TLS handshake was refused) goes to the receiver.
async fn echo_server(
  shape: Shape,
  config: QuicConfig,
) -> (String, mpsc::UnboundedReceiver<Result<(), String>>) {
  let (listener, h3) = match shape {
    Shape::Own => (
      QuicTransport::new(config)
        .listen("127.0.0.1:0")
        .await
        .expect("bind"),
      None,
    ),
    Shape::Shared => {
      // The endpoint presents a public certificate for h3; idm/1 presents the node's.
      let public = NodeCertificate::self_signed("localhost").expect("cert");
      let endpoint = QuicEndpoint::bind(
        "127.0.0.1:0".parse().expect("addr"),
        QuicEndpointConfig::new(Identity::from_certificate(&public).expect("identity")),
      )
      .expect("endpoint");
      let h3 = endpoint.register(Route::h3()).expect("h3");
      let listener = QuicTransport::on_endpoint(config, &endpoint)
        .listen("")
        .await
        .expect("idm/1");
      assert_eq!(
        endpoint.router().client_auth_mode(),
        ClientAuthMode::Optional
      );
      (listener, Some(h3))
    }
  };
  let addr = listener.local_addr();
  let (tx, rx) = mpsc::unbounded_channel();
  tokio::spawn(async move {
    let _h3 = h3;
    loop {
      match listener.accept().await {
        Ok(conn) => {
          let _ = tx.send(Ok(()));
          tokio::spawn(async move {
            if let Ok(frame) = conn.recv().await {
              let _ = conn.send(frame).await;
            }
            // Keep the connection until the dialer closes it.
            while conn.recv().await.is_ok() {}
          });
        }
        Err(TransportError::Closed) => break,
        Err(e) => {
          let _ = tx.send(Err(e.to_string()));
        }
      }
    }
  });
  (addr, rx)
}

/// Dial `addr` with `config` and check one frame comes back.
async fn round_trip(config: QuicConfig, addr: &str) -> Result<(), String> {
  let transport = QuicTransport::try_new(config).map_err(|e| e.to_string())?;
  tokio::time::timeout(WAIT, async {
    let conn = transport.connect(addr).await.map_err(|e| e.to_string())?;
    conn
      .send(Bytes::from_static(b"ping"))
      .await
      .map_err(|e| e.to_string())?;
    let echo = conn.recv().await.map_err(|e| e.to_string())?;
    conn.close().await;
    if echo == Bytes::from_static(b"ping") {
      Ok(())
    } else {
      Err(format!("unexpected echo {echo:?}"))
    }
  })
  .await
  .map_err(|_| "timed out".to_string())?
}

/// The listener's verdict on the next connection attempt.
async fn next_accept(rx: &mut mpsc::UnboundedReceiver<Result<(), String>>) -> Result<(), String> {
  tokio::time::timeout(WAIT, rx.recv())
    .await
    .expect("listener saw the attempt")
    .expect("listener running")
}

#[tokio::test]
async fn mtls_round_trip_with_ca_issued_certificates() {
  for shape in SHAPES {
    mtls_round_trip_on(shape).await;
  }
}

async fn mtls_round_trip_on(shape: Shape) {
  let ca = Ca::new("mesh ca");
  let (addr, mut accepted) = echo_server(shape, node(&ca.issue(), &ca)).await;
  round_trip(node(&ca.issue(), &ca), &addr)
    .await
    .expect("both sides trust the CA");
  assert_eq!(next_accept(&mut accepted).await, Ok(()));
}

/// dist.tls#4: a dialer that presents no certificate (a plain rustls
/// client trusting the server's CA) is refused by the listener.
#[tokio::test]
async fn mtls_rejects_peer_without_client_cert() {
  for shape in SHAPES {
    rejects_peer_without_client_cert_on(shape).await;
  }
}

async fn rejects_peer_without_client_cert_on(shape: Shape) {
  let ca = Ca::new("mesh ca");
  let (addr, mut accepted) = echo_server(shape, node(&ca.issue(), &ca)).await;

  let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
  let mut tls = rustls::ClientConfig::builder_with_provider(provider)
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls13")
    .with_root_certificates(ca.roots())
    .with_no_client_auth();
  // The mesh ALPN, so only the missing certificate can fail.
  tls.alpn_protocols = vec![ALPN_MESH.to_vec()];
  let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic tls");
  let mut endpoint =
    quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("endpoint");
  endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(quic)));
  let dialed = tokio::time::timeout(WAIT, async {
    let conn = endpoint
      .connect(addr.parse().expect("addr"), "cluster")
      .map_err(|e| e.to_string())?
      .await
      .map_err(|e| e.to_string())?;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    send
      .write_all(&[0, 0, 0, 4, b'p', b'i', b'n', b'g'])
      .await
      .map_err(|e| e.to_string())?;
    let mut echo = [0u8; 8];
    recv
      .read_exact(&mut echo)
      .await
      .map_err(|e| e.to_string())?;
    Ok::<_, String>(())
  })
  .await
  .expect("refusal is prompt");
  assert!(dialed.is_err(), "a peer without a certificate got an echo");
  let verdict = next_accept(&mut accepted).await;
  assert!(
    verdict.is_err(),
    "listener accepted a peer without a certificate"
  );

  // The same listener still serves a node with a certificate.
  round_trip(node(&ca.issue(), &ca), &addr)
    .await
    .expect("certified peer");
  assert_eq!(next_accept(&mut accepted).await, Ok(()));
}

/// dist.tls#4: a certificate from another CA is refused in both directions.
#[tokio::test]
async fn mtls_rejects_cert_from_other_ca() {
  for shape in SHAPES {
    rejects_cert_from_other_ca_on(shape).await;
  }
}

async fn rejects_cert_from_other_ca_on(shape: Shape) {
  let (ca, other) = (Ca::new("mesh ca"), Ca::new("other ca"));
  let (addr, mut accepted) = echo_server(shape, node(&ca.issue(), &ca)).await;

  // A client with a certificate the listener does not trust (it trusts the
  // listener, so only client auth can fail).
  let stranger = node(&other.issue(), &ca);
  assert!(round_trip(stranger, &addr).await.is_err());
  assert!(next_accept(&mut accepted).await.is_err());

  // A listener with a certificate the dialer does not trust.
  let (rogue_addr, mut rogue_accepted) = echo_server(shape, node(&other.issue(), &ca)).await;
  let err = round_trip(node(&ca.issue(), &ca), &rogue_addr)
    .await
    .expect_err("dialer verified the server");
  assert!(err.contains("connect failed"), "{err}");
  assert!(next_accept(&mut rogue_accepted).await.is_err());
}

/// dist.tls#5: chain and key from PEM files (PKCS#8, PKCS#1, SEC1) and a CA
/// bundle; each key format completes a mutual-TLS round trip.
#[tokio::test]
async fn pem_chain_and_key_load() {
  for shape in SHAPES {
    pem_chain_and_key_load_on(shape).await;
  }
}

async fn pem_chain_and_key_load_on(shape: Shape) {
  let ca = Ca::new("mesh ca");
  let dir = temp_dir("pem");
  let bundle = dir.join("ca.pem");
  std::fs::write(&bundle, ca.bundle_pem()).expect("write bundle");
  assert_eq!(load_ca_bundle(&bundle).expect("bundle").len(), 1);
  assert_eq!(
    ca_bundle_from_pem(b"no certificates here").err(),
    Some(CertError::NoCertificate)
  );

  let pkcs8 = ca.issue();
  assert!(pkcs8.key_pem.contains("BEGIN PRIVATE KEY"));
  let rsa_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).expect("rsa key");
  let pkcs1 = ca.issue_with(
    &rsa_key,
    pem("RSA PRIVATE KEY", &unwrap_pkcs8(&rsa_key.serialize_der())),
  );
  let ec_key = rcgen::KeyPair::generate().expect("ec key");
  let sec1 = ca.issue_with(
    &ec_key,
    pem("EC PRIVATE KEY", &unwrap_pkcs8(&ec_key.serialize_der())),
  );

  let (addr, mut accepted) = echo_server(shape, node(&ca.issue(), &ca)).await;
  for (format, leaf) in [("pkcs8", &pkcs8), ("pkcs1", &pkcs1), ("sec1", &sec1)] {
    let (chain_path, key_path) = (
      dir.join(format!("{format}.crt")),
      dir.join(format!("{format}.key")),
    );
    std::fs::write(&chain_path, &leaf.chain_pem).expect("write chain");
    std::fs::write(&key_path, &leaf.key_pem).expect("write key");
    let cert = NodeCertificate::from_pem_files(&chain_path, &key_path)
      .unwrap_or_else(|e| panic!("{format}: {e}"));
    assert_eq!(cert.chain().len(), 2, "{format}: leaf and CA");
    assert_eq!(cert.cert_sha256(), leaf.sha256, "{format}: leaf first");
    let config = QuicConfig::from_certificate(&cert)
      .expect("usable key")
      .with_client_verification(load_ca_bundle(&bundle).expect("bundle"));
    round_trip(config, &addr)
      .await
      .unwrap_or_else(|e| panic!("{format}: {e}"));
    assert_eq!(next_accept(&mut accepted).await, Ok(()), "{format}");
  }

  // A key that does not belong to the leaf, a missing key, a missing file.
  assert!(matches!(
    NodeCertificate::from_pem(pkcs8.chain_pem.as_bytes(), sec1.key_pem.as_bytes()),
    Err(CertError::Key(_))
  ));
  assert_eq!(
    NodeCertificate::from_pem(pkcs8.chain_pem.as_bytes(), pkcs8.chain_pem.as_bytes()).err(),
    Some(CertError::NoPrivateKey)
  );
  assert!(matches!(
    NodeCertificate::from_pem_files(dir.join("absent.crt"), dir.join("absent.key")),
    Err(CertError::Read { .. })
  ));
  let _ = std::fs::remove_dir_all(&dir);
}

/// dist.tls#7: the listener keeps running while its certificate files are
/// replaced; once the reloader has polled them, new connections get the new
/// certificate. The dialers pin the leaf to see which one is presented.
#[tokio::test]
async fn rotated_cert_used_for_new_connections() {
  for shape in SHAPES {
    rotated_cert_on(shape).await;
  }
}

async fn rotated_cert_on(shape: Shape) {
  let ca = Ca::new("mesh ca");
  let dir = temp_dir("rotate");
  let (chain_path, key_path) = (dir.join("node.crt"), dir.join("node.key"));
  let old = ca.issue();
  write_atomic(&chain_path, &old.chain_pem);
  write_atomic(&key_path, &old.key_pem);

  let reloader = Arc::new(CertReloader::new(&chain_path, &key_path).expect("load"));
  assert_eq!(reloader.cert_sha256(), old.sha256);
  assert!(!reloader.reload_if_changed().expect("unchanged"));
  let polling = reloader.spawn_polling(Duration::from_millis(20));
  let server = QuicConfig::from_reloader(&reloader).with_client_verification(ca.roots());
  let (addr, mut accepted) = echo_server(shape, server).await;

  let pinned = |sha: [u8; 32]| node(&ca.issue(), &ca).with_pinned_server_cert_sha256(sha);
  round_trip(pinned(old.sha256), &addr)
    .await
    .expect("old certificate before the rotation");
  assert_eq!(next_accept(&mut accepted).await, Ok(()));

  let new = ca.issue();
  write_atomic(&chain_path, &new.chain_pem);
  write_atomic(&key_path, &new.key_pem);
  tokio::time::timeout(WAIT, async {
    while reloader.cert_sha256() != new.sha256 {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("the poller picked up the new files");

  round_trip(pinned(new.sha256), &addr)
    .await
    .expect("new certificate without a restart");
  assert_eq!(next_accept(&mut accepted).await, Ok(()));
  assert!(
    round_trip(pinned(old.sha256), &addr).await.is_err(),
    "the old certificate is no longer presented"
  );

  // A broken file keeps the current certificate.
  drop(polling);
  write_atomic(&key_path, "not a key");
  assert!(reloader.reload_if_changed().is_err());
  assert_eq!(reloader.cert_sha256(), new.sha256);
  let _ = std::fs::remove_dir_all(&dir);
}

/// ADR 0013 R10: both QUIC opt-outs are recorded when taken.
#[test]
fn dangerous_accept_any_server_cert_is_recorded_in_posture() {
  let recorded = |id: &str| {
    dangerous_opt_outs()
      .into_iter()
      .find(|(found, _)| *found == id)
      .map(|(_, reopens)| reopens)
  };
  assert_eq!(recorded(DANGEROUS_ACCEPT_ANY_SERVER_CERT), None);
  let config = QuicConfig::self_signed("node-a")
    .expect("cert")
    .dangerous_accept_any_server_cert();
  let reopens = recorded(DANGEROUS_ACCEPT_ANY_SERVER_CERT).expect("recorded");
  assert!(reopens.contains("any server certificate"), "{reopens}");

  assert_eq!(recorded(DANGEROUS_NO_CLIENT_AUTH), None);
  let _config = config.dangerous_no_client_auth();
  let reopens = recorded(DANGEROUS_NO_CLIENT_AUTH).expect("recorded");
  assert!(reopens.contains("client certificate"), "{reopens}");
}
