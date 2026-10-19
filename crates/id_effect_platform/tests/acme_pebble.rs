//! ACME TLS-ALPN-01 end to end against Pebble, Let's Encrypt's test directory, in a container
//! pinned by digest (ADR 0014 D8): Pebble validates the challenge by connecting to the test's
//! TCP TLS listener, which answers through `Acme::serve_tls_alpn01`; issued and renewed
//! certificates are installed into the identity that listener presents.
//!
//! Pebble runs with host networking on ports the test picks, and resolves the test domain to
//! 127.0.0.1 (`--add-host`). Without Docker the tests print `SKIP` and pass, unless
//! `ID_EFFECT_REQUIRE_DOCKER=1`, where a missing Docker fails them.

#![cfg(all(feature = "acme", unix))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use id_effect_platform::tls::acme::{Acme, AcmeConfig, AcmeError, is_acme_challenge};
use id_effect_platform::tls::{
  Identity, NodeCertificate, client_tls_config, server_tls_config_with, sha256,
};
use rustls::{ClientConnection, ServerConnection, StreamOwned};

/// `ghcr.io/letsencrypt/pebble:2.8.0`, pinned by digest.
const IMAGE: &str = "ghcr.io/letsencrypt/pebble@sha256:d9080f68f6cb6af8d82134ab26de0aaaf312ac9cba42aecc6d3aede6cb63007b";
const DOMAIN: &str = "acme.id-effect.test";
const BLOCKED: &str = "blocked.id-effect.test";

fn free_port() -> u16 {
  TcpListener::bind("127.0.0.1:0")
    .expect("bind")
    .local_addr()
    .expect("addr")
    .port()
}

/// A running Pebble container; removed on drop.
struct Pebble {
  name: String,
  directory: String,
  root: PathBuf,
  _dir: tempfile::TempDir,
}

impl Drop for Pebble {
  fn drop(&mut self) {
    let _ = Command::new("docker")
      .args(["rm", "-f", &self.name])
      .output();
  }
}

fn docker(args: &[&str]) -> Result<String, String> {
  let out = Command::new("docker")
    .args(args)
    .output()
    .map_err(|e| format!("docker: {e}"))?;
  if out.status.success() {
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
  } else {
    Err(String::from_utf8_lossy(&out.stderr).into_owned())
  }
}

/// Pebble validating TLS-ALPN-01 on 127.0.0.1:`tls_port`, issuing certificates valid for
/// `validity`; `None` (after printing SKIP) without Docker.
fn pebble(tls_port: u16, validity: Duration, authz_reuse: u8) -> Option<Pebble> {
  if let Err(e) = docker(&["version", "--format", "{{.Server.Version}}"]) {
    if std::env::var("ID_EFFECT_REQUIRE_DOCKER").as_deref() == Ok("1") {
      panic!("ID_EFFECT_REQUIRE_DOCKER=1 but Docker is not available: {e}");
    }
    eprintln!("SKIP acme_pebble: Docker is not available ({e})");
    return None;
  }
  let dir = tempfile::tempdir().expect("dir");
  let (listen, management) = (free_port(), free_port());
  let config = serde_json::json!({
    "pebble": {
      "listenAddress": format!("127.0.0.1:{listen}"),
      "managementListenAddress": format!("127.0.0.1:{management}"),
      "certificate": "test/certs/localhost/cert.pem",
      "privateKey": "test/certs/localhost/key.pem",
      "httpPort": free_port(),
      "tlsPort": tls_port,
      "ocspResponderURL": "",
      "externalAccountBindingRequired": false,
      "domainBlocklist": [BLOCKED],
      "retryAfter": { "authz": 1, "order": 1 },
      "profiles": {
        "default": { "description": "id_effect test", "validityPeriod": validity.as_secs() }
      }
    }
  });
  let config_path = dir.path().join("pebble.json");
  std::fs::write(&config_path, config.to_string()).expect("config");
  let name = format!("id-effect-pebble-{}-{listen}", std::process::id());
  docker(&[
    "run",
    "-d",
    "--name",
    &name,
    "--network",
    "host",
    "--add-host",
    &format!("{DOMAIN}:127.0.0.1"),
    "-e",
    "PEBBLE_VA_NOSLEEP=1",
    "-e",
    "PEBBLE_WFE_NONCEREJECT=0",
    "-e",
    &format!("PEBBLE_AUTHZREUSE={authz_reuse}"),
    "-v",
    &format!("{}:/test/config/id-effect.json:ro", config_path.display()),
    IMAGE,
    "-config",
    "/test/config/id-effect.json",
  ])
  .unwrap_or_else(|e| panic!("pebble did not start: {e}"));
  let pebble = Pebble {
    directory: format!("https://localhost:{listen}/dir"),
    root: dir.path().join("minica.pem"),
    name,
    _dir: dir,
  };
  let deadline = Instant::now() + Duration::from_secs(30);
  while TcpStream::connect(("127.0.0.1", listen)).is_err() {
    assert!(
      Instant::now() < deadline,
      "pebble not listening: {}",
      docker(&["logs", &pebble.name]).unwrap_or_default()
    );
    std::thread::sleep(Duration::from_millis(50));
  }
  docker(&[
    "cp",
    &format!("{}:/test/certs/pebble.minica.pem", pebble.name),
    &pebble.root.display().to_string(),
  ])
  .expect("minica");
  Some(pebble)
}

/// The application's TCP TLS listener: answers TLS-ALPN-01 through the current `Acme` (which
/// the test can swap, as a restarted process would) and echoes everything else.
struct Listener {
  port: u16,
  config: Arc<RwLock<Arc<rustls::ServerConfig>>>,
}

impl Listener {
  fn start() -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let placeholder = Identity::from_certificate(&NodeCertificate::self_signed(DOMAIN).expect("c"))
      .expect("identity");
    let config = Arc::new(RwLock::new(Arc::new(
      server_tls_config_with(Arc::new(placeholder), None).expect("config"),
    )));
    let shared = Arc::clone(&config);
    std::thread::spawn(move || {
      for socket in listener.incoming() {
        let Ok(socket) = socket else { return };
        let config = Arc::clone(&shared.read().expect("config"));
        std::thread::spawn(move || serve(config, socket));
      }
    });
    Self { port, config }
  }

  fn serve_with(&self, acme: &Acme) {
    let mut config = server_tls_config_with(Arc::new(acme.resolver()), None).expect("config");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    acme.serve_tls_alpn01(&mut config);
    *self.config.write().expect("config") = Arc::new(config);
  }
}

fn serve(config: Arc<rustls::ServerConfig>, socket: TcpStream) {
  let mut tls = StreamOwned::new(ServerConnection::new(config).expect("conn"), socket);
  while tls.conn.is_handshaking() {
    if tls.conn.complete_io(&mut tls.sock).is_err() {
      return;
    }
  }
  if is_acme_challenge(tls.conn.alpn_protocol()) {
    // The validator has what it came for; serve it nothing.
    tls.conn.send_close_notify();
    let _ = tls.conn.complete_io(&mut tls.sock);
    return;
  }
  let mut buf = [0u8; 64];
  while let Ok(n @ 1..) = tls.read(&mut buf) {
    if tls.write_all(&buf[..n]).and_then(|()| tls.flush()).is_err() {
      return;
    }
  }
}

/// Dial the listener as a client pinning `expected`; returns what it presented and checks
/// the connection carries data.
fn presented(port: u16, expected: [u8; 32]) -> [u8; 32] {
  let me = Identity::from_certificate(&NodeCertificate::self_signed("client").expect("c"))
    .expect("identity");
  let config = client_tls_config(&me, None, &[expected]).expect("client");
  let conn =
    ClientConnection::new(Arc::new(config), DOMAIN.try_into().expect("name")).expect("client");
  let mut tls = StreamOwned::new(conn, TcpStream::connect(("127.0.0.1", port)).expect("dial"));
  tls.write_all(b"ping").expect("write");
  tls.flush().expect("flush");
  let mut back = [0u8; 4];
  tls.read_exact(&mut back).expect("read");
  assert_eq!(&back, b"ping");
  sha256(&tls.conn.peer_certificates().expect("certs")[0])
}

/// Enables every event, so the log statements are evaluated (and must not panic).
struct Everything;

impl tracing::Subscriber for Everything {
  fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
    true
  }
  fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(1)
  }
  fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
  fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
  fn event(&self, _: &tracing::Event<'_>) {}
  fn enter(&self, _: &tracing::span::Id) {}
  fn exit(&self, _: &tracing::span::Id) {}
}

fn mode(path: &Path) -> u32 {
  use std::os::unix::fs::PermissionsExt;
  std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

fn config(pebble: &Pebble, domain: &str, cache: &Path) -> AcmeConfig {
  AcmeConfig::new([domain], cache)
    .directory_url(&pebble.directory)
    .directory_root(&pebble.root)
    .contact("mailto:ops@id-effect.test")
    .agree_to_terms_of_service()
    .order_timeout(Duration::from_secs(30))
    .retry_after_failure(Duration::from_secs(1))
}

#[tokio::test(flavor = "multi_thread")]
async fn acme_issues_and_renews_against_pebble() {
  let listener = Listener::start();
  let lifetime = Duration::from_secs(30);
  let Some(pebble) = pebble(listener.port, lifetime, 0) else {
    return;
  };
  let _logs = tracing::subscriber::set_default(Everything);
  let cache = tempfile::tempdir().expect("cache");
  let acme = Acme::new(config(&pebble, DOMAIN, cache.path())).expect("acme");
  listener.serve_with(&acme);
  let placeholder = acme.identity().cert_sha256();

  // First issuance: validated over TLS-ALPN-01 on the listener, installed into the identity.
  let first = acme.issue().await.expect("issued");
  assert_ne!(first, placeholder);
  assert_eq!(acme.identity().cert_sha256(), first);
  assert_eq!(presented(listener.port, first), first);
  let chain = acme.identity().current().cert.clone();
  assert!(chain.len() >= 2, "leaf and Pebble's intermediate");
  let stats = acme.stats();
  assert_eq!((stats.issued, stats.failed), (1, 0));
  let (not_after, renew_at) = (
    stats.not_after.expect("not_after"),
    stats.renew_at.expect("renew_at"),
  );
  assert!(renew_at < not_after && not_after <= SystemTime::now() + lifetime + lifetime);

  // Key material on disk: owner-only.
  let account = std::fs::read_dir(cache.path())
    .expect("cache")
    .map(|e| e.expect("entry").path())
    .find(|p| {
      p.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("account-"))
    })
    .expect("account file");
  assert_eq!(mode(&account), 0o600, "account key");
  assert_eq!(
    mode(&cache.path().join("key.pem")),
    0o600,
    "certificate key"
  );
  assert_eq!(mode(cache.path()), 0o700);
  assert!(!format!("{acme:?}").contains("PRIVATE"));

  // Renewal: the background task renews at two thirds of the 30 s lifetime.
  let task = acme.spawn();
  tokio::time::timeout(Duration::from_secs(60), async {
    while acme.stats().issued < 2 {
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  })
  .await
  .expect("renewed");
  assert!(SystemTime::now() >= renew_at, "not renewed early");
  drop(task);
  let second = acme.identity().cert_sha256();
  assert_ne!(second, first);
  assert_eq!(presented(listener.port, second), second);

  // A restart presents the cached certificate and reuses the stored account.
  let restarted = Acme::new(config(&pebble, DOMAIN, cache.path())).expect("restart");
  assert_eq!(restarted.identity().cert_sha256(), second);
  listener.serve_with(&restarted);
  let third = restarted
    .issue()
    .await
    .expect("issued with the stored account");
  assert_eq!(presented(listener.port, third), third);
}

/// With authorization reuse on, a second order for the same domain finds its authorization
/// already valid and needs no challenge.
#[tokio::test(flavor = "multi_thread")]
async fn acme_reuses_valid_authorizations_against_pebble() {
  let listener = Listener::start();
  let Some(pebble) = pebble(listener.port, Duration::from_secs(3600), 100) else {
    return;
  };
  let cache = tempfile::tempdir().expect("cache");
  let acme = Acme::new(config(&pebble, DOMAIN, cache.path())).expect("acme");
  listener.serve_with(&acme);
  let first = acme.issue().await.expect("issued");
  // Nothing answers challenges any more: only a reused authorization can succeed.
  let elsewhere = tempfile::tempdir().expect("cache");
  let silent = Acme::new(config(&pebble, DOMAIN, elsewhere.path())).expect("acme");
  listener.serve_with(&silent);
  let second = acme.issue().await.expect("reused authorization");
  assert_ne!(first, second);
  assert_eq!(acme.stats().issued, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn acme_failure_against_pebble_keeps_current_cert() {
  let listener = Listener::start();
  let Some(pebble) = pebble(listener.port, Duration::from_secs(3600), 0) else {
    return;
  };
  let _logs = tracing::subscriber::set_default(Everything);

  // The directory refuses the order.
  let cache = tempfile::tempdir().expect("cache");
  let blocked = Acme::new(config(&pebble, BLOCKED, cache.path())).expect("acme");
  let before = blocked.identity().cert_sha256();
  let refused = blocked.issue().await.expect_err("blocked domain");
  assert!(matches!(refused, AcmeError::Order(_)), "{refused}");
  assert_eq!(blocked.identity().cert_sha256(), before);
  assert_eq!((blocked.stats().issued, blocked.stats().failed), (0, 1));

  // The challenge is not answered (the listener serves another Acme): validation fails.
  let answering = Acme::new(config(
    &pebble,
    DOMAIN,
    tempfile::tempdir().expect("c").path(),
  ))
  .expect("acme");
  listener.serve_with(&answering);
  let cache = tempfile::tempdir().expect("cache");
  let unanswered = Acme::new(config(&pebble, DOMAIN, cache.path())).expect("acme");
  let before = unanswered.identity().cert_sha256();
  let failed = unanswered
    .issue()
    .await
    .expect_err("challenge not answered");
  assert!(matches!(failed, AcmeError::Order(_)), "{failed}");
  assert_eq!(unanswered.identity().cert_sha256(), before);
  assert_eq!(unanswered.stats().failed, 1);
  assert!(!cache.path().join("cert.pem").exists());
}
