//! Certificate reload on the shared listeners (ADR 0014 D8, `dist.tls#7`): one `Identity`,
//! fed by a `CertReloader` (or `Identity::install`), drives the QUIC endpoint and a TCP TLS
//! listener; new connections present the new certificate, connections already open keep
//! theirs and keep working.

#![cfg(feature = "tls")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;

use id_effect_platform::tls::{
  CertReloader, Identity, NodeCertificate, ca_bundle_from_pem, server_tls_config, sha256,
};
use rustls::{ClientConnection, ServerConnection, StreamOwned};

/// A throwaway certificate authority.
struct Ca {
  cert: rcgen::Certificate,
  issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl Ca {
  fn new() -> Self {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
      .distinguished_name
      .push(rcgen::DnType::CommonName, "reload ca");
    params.key_usages = vec![
      rcgen::KeyUsagePurpose::KeyCertSign,
      rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().expect("ca key");
    let cert = params.self_signed(&key).expect("ca cert");
    let issuer = rcgen::Issuer::new(params, key);
    Self { cert, issuer }
  }

  fn roots(&self) -> rustls::RootCertStore {
    ca_bundle_from_pem(self.cert.pem().as_bytes()).expect("roots")
  }

  /// A "localhost" leaf as (chain PEM, key PEM).
  fn issue(&self) -> (String, String) {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("params");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = params.signed_by(&key, &self.issuer).expect("leaf");
    (
      format!("{}{}", cert.pem(), self.cert.pem()),
      key.serialize_pem(),
    )
  }

  /// Issue a leaf into `dir/{cert,key}.pem`; returns its fingerprint.
  fn write(&self, dir: &Path) -> [u8; 32] {
    let (chain, key) = self.issue();
    std::fs::write(dir.join("cert.pem"), &chain).expect("chain");
    std::fs::write(dir.join("key.pem"), &key).expect("key");
    NodeCertificate::from_pem(chain.as_bytes(), key.as_bytes())
      .expect("pem")
      .cert_sha256()
  }
}

/// A TCP TLS echo listener presenting `identity` (read on every handshake).
fn tcp_listener(identity: &Identity) -> SocketAddr {
  let config = Arc::new(server_tls_config(identity, None, &[]).expect("server config"));
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  std::thread::spawn(move || {
    for socket in listener.incoming() {
      let Ok(socket) = socket else { return };
      let config = Arc::clone(&config);
      std::thread::spawn(move || {
        let mut tls = StreamOwned::new(ServerConnection::new(config).expect("conn"), socket);
        let mut buf = [0u8; 256];
        while let Ok(n @ 1..) = tls.read(&mut buf) {
          if tls.write_all(&buf[..n]).and_then(|()| tls.flush()).is_err() {
            return;
          }
        }
      });
    }
  });
  addr
}

type TcpTls = StreamOwned<ClientConnection, TcpStream>;

fn client_tls(ca: &Ca) -> rustls::ClientConfig {
  rustls::ClientConfig::builder_with_provider(id_effect_platform::tls::provider())
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls13")
    .with_root_certificates(ca.roots())
    .with_no_client_auth()
}

/// Dial the TCP listener, verifying it against `ca`; returns the stream and the fingerprint
/// of the certificate it presented.
fn tcp_dial(ca: &Ca, addr: SocketAddr) -> (TcpTls, [u8; 32]) {
  let conn = ClientConnection::new(
    Arc::new(client_tls(ca)),
    "localhost".try_into().expect("name"),
  )
  .expect("client");
  let mut tls = StreamOwned::new(conn, TcpStream::connect(addr).expect("connect"));
  while tls.conn.is_handshaking() {
    tls.conn.complete_io(&mut tls.sock).expect("handshake");
  }
  let presented = sha256(&tls.conn.peer_certificates().expect("certs")[0]);
  (tls, presented)
}

#[test]
fn reload_swaps_identity_on_tcp() {
  let (ca, dir) = (Ca::new(), tempfile::tempdir().expect("dir"));
  let first = ca.write(dir.path());
  let reloader =
    CertReloader::new(dir.path().join("cert.pem"), dir.path().join("key.pem")).expect("reloader");
  let addr = tcp_listener(&reloader.identity());
  assert_eq!(tcp_dial(&ca, addr).1, first);

  let second = ca.write(dir.path());
  assert!(reloader.reload_if_changed().expect("reload"));
  assert_eq!(tcp_dial(&ca, addr).1, second);

  // By hand (what ACME does): the same slot.
  let (chain, key) = ca.issue();
  let third = NodeCertificate::from_pem(chain.as_bytes(), key.as_bytes()).expect("pem");
  assert_eq!(
    reloader.identity().install(&third).expect("install"),
    third.cert_sha256()
  );
  assert_eq!(tcp_dial(&ca, addr).1, third.cert_sha256());
  assert_eq!(reloader.cert_sha256(), third.cert_sha256());
}

#[cfg(feature = "quic")]
mod quic {
  use std::time::Duration;

  use id_effect_platform::quic::{
    ALPN_H3, AlpnRoute, QuicEndpoint, QuicEndpointConfig, QuicLimits, Route, client_config,
  };
  use rustls::pki_types::CertificateDer;

  use super::*;

  const WAIT: Duration = Duration::from_secs(5);

  /// The endpoint presenting `identity`, with an `h3` route whose connections echo every
  /// bidirectional stream.
  fn endpoint(identity: &Identity) -> (QuicEndpoint, AlpnRoute) {
    let endpoint = QuicEndpoint::bind(
      "127.0.0.1:0".parse().expect("addr"),
      QuicEndpointConfig::new(identity.clone()),
    )
    .expect("bind");
    let route = endpoint.register(Route::h3()).expect("h3");
    (endpoint, route)
  }

  /// Accept the next connection on `route` and echo its streams in the background.
  async fn serve_next(route: &AlpnRoute) {
    let conn = tokio::time::timeout(WAIT, route.accept())
      .await
      .expect("delivered")
      .expect("running")
      .expect("accepted");
    tokio::spawn(async move {
      while let Ok((mut send, mut recv)) = conn.accept_bi().await {
        let message = recv.read_to_end(1024).await.expect("read");
        send.write_all(&message).await.expect("write");
        send.finish().expect("finish");
      }
    });
  }

  /// Dial `addr` over QUIC (ALPN `h3`), verifying against `ca`; returns the connection and
  /// the fingerprint of the certificate the endpoint presented.
  async fn quic_dial(
    ca: &Ca,
    client: &quinn::Endpoint,
    route: &AlpnRoute,
    addr: SocketAddr,
  ) -> (quinn::Connection, [u8; 32]) {
    let config = client_config(client_tls(ca), &[ALPN_H3], &QuicLimits::default()).expect("cfg");
    let connecting = client
      .connect_with(config, addr, "localhost")
      .expect("connect");
    let (conn, ()) = tokio::join!(
      async {
        tokio::time::timeout(WAIT, connecting)
          .await
          .expect("in time")
          .expect("handshake")
      },
      serve_next(route)
    );
    let certs = conn
      .peer_identity()
      .expect("identity")
      .downcast::<Vec<CertificateDer<'static>>>()
      .expect("certs");
    let presented = sha256(&certs[0]);
    (conn, presented)
  }

  async fn quic_echo(conn: &quinn::Connection, message: &[u8]) -> Vec<u8> {
    let (mut send, mut recv) = conn.open_bi().await.expect("open");
    send.write_all(message).await.expect("write");
    send.finish().expect("finish");
    tokio::time::timeout(WAIT, recv.read_to_end(1024))
      .await
      .expect("in time")
      .expect("read")
  }

  fn tcp_echo(tls: &mut TcpTls, message: &[u8]) -> Vec<u8> {
    tls.write_all(message).expect("write");
    tls.flush().expect("flush");
    let mut back = vec![0u8; message.len()];
    tls.read_exact(&mut back).expect("read");
    back
  }

  fn quic_client() -> quinn::Endpoint {
    quinn::Endpoint::client("127.0.0.1:0".parse().expect("addr")).expect("client")
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn reload_swaps_identity_on_endpoint_and_tcp() {
    let (ca, dir) = (Ca::new(), tempfile::tempdir().expect("dir"));
    let first = ca.write(dir.path());
    let reloader = Arc::new(
      CertReloader::new(dir.path().join("cert.pem"), dir.path().join("key.pem")).expect("reloader"),
    );
    let (endpoint, route) = endpoint(&reloader.identity());
    let tcp = tcp_listener(&reloader.identity());
    let client = quic_client();
    assert_eq!(
      quic_dial(&ca, &client, &route, endpoint.local_addr())
        .await
        .1,
      first
    );
    assert_eq!(tcp_dial(&ca, tcp).1, first);

    // The rotation is picked up by the polling task, with no restart of either listener.
    let polling = reloader.spawn_polling(Duration::from_millis(20));
    let second = ca.write(dir.path());
    tokio::time::timeout(WAIT, async {
      while reloader.cert_sha256() != second {
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .expect("reloaded");
    drop(polling);
    assert_eq!(
      quic_dial(&ca, &client, &route, endpoint.local_addr())
        .await
        .1,
      second
    );
    assert_eq!(tcp_dial(&ca, tcp).1, second);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn existing_connections_survive_reload() {
    let (ca, dir) = (Ca::new(), tempfile::tempdir().expect("dir"));
    let first = ca.write(dir.path());
    let reloader =
      CertReloader::new(dir.path().join("cert.pem"), dir.path().join("key.pem")).expect("reloader");
    let (endpoint, route) = endpoint(&reloader.identity());
    let tcp = tcp_listener(&reloader.identity());
    let client = quic_client();
    let (quic_old, quic_seen) = quic_dial(&ca, &client, &route, endpoint.local_addr()).await;
    let (mut tcp_old, tcp_seen) = tcp_dial(&ca, tcp);
    assert_eq!((quic_seen, tcp_seen), (first, first));
    assert_eq!(quic_echo(&quic_old, b"before").await, b"before");
    assert_eq!(tcp_echo(&mut tcp_old, b"before"), b"before");

    let second = ca.write(dir.path());
    assert!(reloader.reload_if_changed().expect("reload"));

    // Open connections carry on, on the certificate they were established with.
    assert_eq!(quic_echo(&quic_old, b"after").await, b"after");
    assert_eq!(tcp_echo(&mut tcp_old, b"after"), b"after");
    let still = quic_old
      .peer_identity()
      .expect("identity")
      .downcast::<Vec<CertificateDer<'static>>>()
      .expect("certs");
    assert_eq!(sha256(&still[0]), first);
    assert_eq!(
      sha256(&tcp_old.conn.peer_certificates().expect("certs")[0]),
      first
    );
    assert!(quic_old.close_reason().is_none(), "not cut");

    // New ones get the new certificate.
    let (quic_new, quic_seen) = quic_dial(&ca, &client, &route, endpoint.local_addr()).await;
    assert_eq!(quic_seen, second);
    assert_eq!(quic_echo(&quic_new, b"new").await, b"new");
    let (mut tcp_new, tcp_seen) = tcp_dial(&ca, tcp);
    assert_eq!(tcp_seen, second);
    assert_eq!(tcp_echo(&mut tcp_new, b"new"), b"new");
  }
}
