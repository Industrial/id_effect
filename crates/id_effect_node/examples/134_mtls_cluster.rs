//! Example 134 — a two-node mesh over QUIC with mutual TLS: operator-issued
//! certificates from PEM files, a peer from another CA refused, and a
//! certificate rotated without restarting the listener.
//!
//! Run: `cargo run -p id_effect_node --features quic --example 134_mtls_cluster`

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use id_effect::observability::posture::dangerous_opt_outs;
use id_effect_node::handshake::Cookie;
use id_effect_node::id::NodeId;
use id_effect_node::session::Session;
use id_effect_node::transport::certs::{CertReloader, NodeCertificate, load_ca_bundle};
use id_effect_node::transport::quic::{QuicConfig, QuicTransport};
use id_effect_node::transport::{Listener, Transport};
use id_effect_node::wire::Frame;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Stands in for the operator's PKI: a CA, and node certificates for the
/// server name every node verifies (`"cluster"`), valid for both TLS roles.
struct Pki {
  cert: rcgen::Certificate,
  issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl Pki {
  fn new(name: &str) -> Result<Self, Error> {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, name);
    params.key_usages = vec![
      rcgen::KeyUsagePurpose::KeyCertSign,
      rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    let issuer = rcgen::Issuer::new(params, key);
    Ok(Self { cert, issuer })
  }

  /// Write `<name>.crt` (leaf, then CA) and `<name>.key` (PKCS#8) to `dir`.
  fn issue(&self, dir: &Path, name: &str) -> Result<(), Error> {
    let mut params = rcgen::CertificateParams::new(vec!["cluster".to_string()])?;
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, name);
    params.extended_key_usages = vec![
      rcgen::ExtendedKeyUsagePurpose::ServerAuth,
      rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let key = rcgen::KeyPair::generate()?;
    let cert = params.signed_by(&key, &self.issuer)?;
    write_atomic(
      &dir.join(format!("{name}.crt")),
      &format!("{}{}", cert.pem(), self.cert.pem()),
    )?;
    write_atomic(&dir.join(format!("{name}.key")), &key.serialize_pem())?;
    Ok(())
  }
}

fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
  let tmp = path.with_extension("tmp");
  std::fs::write(&tmp, contents)?;
  std::fs::rename(&tmp, path)
}

fn short(fingerprint: [u8; 32]) -> String {
  fingerprint[..6]
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect()
}

/// Accept one connection and answer the cookie handshake as `me`.
async fn respond_once(listener: &dyn Listener, me: &NodeId, cookie: &Cookie) -> Result<(), Error> {
  let conn = listener.accept().await?;
  let session = Session::respond(conn, me, cookie).await?;
  if let Frame::Data(bytes) = session.recv().await? {
    println!(
      "b: authenticated {} and received {:?}",
      session.peer(),
      String::from_utf8_lossy(&bytes)
    );
  }
  session.send(&Frame::Data(b"welcome".to_vec())).await?;
  // Dropping the session closes the connection at once; let the dialer
  // read the reply and close first.
  while session.recv().await.is_ok() {}
  Ok(())
}

/// Dial `addr` as `me`, authenticate with the cookie, and exchange a frame.
async fn dial(
  config: QuicConfig,
  addr: &str,
  me: &NodeId,
  cookie: &Cookie,
) -> Result<String, Error> {
  let conn = QuicTransport::try_new(config)?.connect(addr).await?;
  let session = Session::initiate(conn, me, cookie).await?;
  session.send(&Frame::Data(b"hello".to_vec())).await?;
  let reply = session.recv().await?;
  session.close().await;
  match reply {
    Frame::Data(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
    other => Err(format!("unexpected frame {other:?}").into()),
  }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
  let dir = std::env::temp_dir().join(format!("id-effect-134-{}", std::process::id()));
  std::fs::create_dir_all(&dir)?;
  let pki = Pki::new("mesh ca")?;
  pki.issue(&dir, "node-a")?;
  pki.issue(&dir, "node-b")?;
  std::fs::write(dir.join("ca.pem"), pki.cert.pem())?;

  // What a node does at start: load its certificate, key and CA bundle.
  let roots = load_ca_bundle(dir.join("ca.pem"))?;
  let a_cert = NodeCertificate::from_pem_files(dir.join("node-a.crt"), dir.join("node-a.key"))?;
  let a = QuicConfig::from_certificate(&a_cert)?.with_client_verification(roots.clone());
  // Node b reloads its files when they change (polled; no restart).
  let b_files = Arc::new(CertReloader::new(
    dir.join("node-b.crt"),
    dir.join("node-b.key"),
  )?);
  let _polling = b_files.spawn_polling(Duration::from_millis(50));
  let b = QuicConfig::from_reloader(&b_files).with_client_verification(roots.clone());

  let cookie = Cookie::try_new("an-example-cluster-cookie-of-32-bytes+")?;
  let (a_id, b_id) = (
    NodeId::from_parts("a@mesh", 1),
    NodeId::from_parts("b@mesh", 1),
  );
  let listener = QuicTransport::new(b).listen("127.0.0.1:0").await?;
  let addr = listener.local_addr();
  println!(
    "b: listening on {addr} with certificate {}",
    short(b_files.cert_sha256())
  );

  // 1. Mutual TLS, then the cookie handshake.
  let (served, reply) = tokio::join!(
    respond_once(listener.as_ref(), &b_id, &cookie),
    dial(a, &addr, &a_id, &cookie)
  );
  served?;
  println!("a: b replied {:?}", reply?);

  // 2. A node with a certificate from another CA is refused during TLS,
  //    before it can try the cookie.
  let rogue = Pki::new("rogue ca")?;
  rogue.issue(&dir, "rogue")?;
  let rogue_cert = NodeCertificate::from_pem_files(dir.join("rogue.crt"), dir.join("rogue.key"))?;
  let stranger = QuicConfig::from_certificate(&rogue_cert)?.with_client_verification(roots.clone());
  let rogue_id = NodeId::from_parts("rogue@mesh", 1);
  let (accepted, dialed) = tokio::join!(
    respond_once(listener.as_ref(), &b_id, &cookie),
    tokio::time::timeout(
      Duration::from_secs(5),
      dial(stranger, &addr, &rogue_id, &cookie)
    )
  );
  let Err(refusal) = accepted else {
    return Err("b accepted a peer from another CA".into());
  };
  if let Ok(Ok(_)) = dialed {
    return Err("the rogue node completed a session".into());
  }
  println!("b: refused a peer from another CA ({refusal})");

  // 3. Rotate b's certificate on disk; the running listener picks it up.
  let before = b_files.cert_sha256();
  pki.issue(&dir, "node-b")?;
  tokio::time::timeout(Duration::from_secs(5), async {
    while b_files.cert_sha256() == before {
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await?;
  println!(
    "b: rotated certificate {} -> {}",
    short(before),
    short(b_files.cert_sha256())
  );
  let a = QuicConfig::from_certificate(&a_cert)?
    .with_client_verification(roots)
    .with_pinned_server_cert_sha256(b_files.cert_sha256());
  let (served, reply) = tokio::join!(
    respond_once(listener.as_ref(), &b_id, &cookie),
    dial(a, &addr, &a_id, &cookie)
  );
  served?;
  println!(
    "a: reconnected to the new certificate; b replied {:?}",
    reply?
  );

  // 4. Nothing was weakened: the posture registry is empty.
  assert!(dangerous_opt_outs().is_empty());
  println!("posture: no dangerous opt-outs recorded");

  let _ = std::fs::remove_dir_all(&dir);
  Ok(())
}
