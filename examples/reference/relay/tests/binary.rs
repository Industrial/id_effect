//! The `relay` binary: configured from the environment, it creates its schema, checks its
//! posture, serves until SIGTERM and stops cleanly; a missing setting stops it at once.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{http_config, postgres_url, run};
use id_effect_kit::platform::http::{HttpClient, HttpRequest, ReqwestHttpClient};
use id_effect_kit::platform::tls::NodeCertificate;

const RELAY: &str = env!("CARGO_BIN_EXE_relay");

/// Waits until the child's stderr file holds `marker` (or the child exits), for 60 s.
fn wait_for_log(child: &mut Child, log: &Path, marker: &str) -> String {
  let deadline = Instant::now() + Duration::from_secs(60);
  loop {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    if text.contains(marker) {
      return text;
    }
    if let Some(status) = child.try_wait().unwrap() {
      panic!("relay exited ({status}) before `{marker}`:\n{text}");
    }
    assert!(Instant::now() < deadline, "no `{marker}` in:\n{text}");
    std::thread::sleep(Duration::from_millis(50));
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_binary_serves_until_sigterm() {
  let Some(url) = postgres_url("relay_binary_serves_until_sigterm").await else {
    return;
  };
  let dir = std::env::temp_dir().join(format!("relay-binary-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  // The `id-effect dev` certificate serves clients (the dev profile allows it) and the mesh.
  let dev =
    id_effect_tool::dev::cert::ensure(&dir.join("cert"), std::time::SystemTime::now()).unwrap();
  let log = dir.join("stderr");
  let mut child = Command::new(RELAY)
    .current_dir(&dir)
    .env("ID_EFFECT_PROFILE", "dev")
    .env("RELAY_NODE", "a@relay")
    .env("RELAY_BIND", "127.0.0.1:0")
    .env("RELAY_MESH_BIND", "127.0.0.1:0")
    // A peer that is not there: logged, the node serves anyway.
    .env("RELAY_PEERS", "127.0.0.1:9")
    .env("TLS_CERT_FILE", &dev.cert)
    .env("TLS_KEY_FILE", &dev.key)
    .env("RELAY_MESH_CERT_FILE", &dev.cert)
    .env("RELAY_MESH_KEY_FILE", &dev.key)
    .env("RELAY_MESH_TRUSTED", dev.sha256_hex())
    .env("RELAY_MESH_COOKIE", "relay-binary-test-cookie-of-32-bytes!")
    .env("RELAY_ORIGINS", "https://chat.relay.test")
    .env(
      "RELAY_TOKEN_SECRET",
      "relay-binary-test-secret-of-32-bytes!!",
    )
    .env("RELAY_TOKEN_ISSUER", common::ISSUER)
    .env("RELAY_DATABASE_URL", &url)
    .stdout(Stdio::null())
    .stderr(std::fs::File::create(&log).unwrap())
    .spawn()
    .expect("relay starts");
  let text = tokio::task::block_in_place(|| wait_for_log(&mut child, &log, "(TCP and UDP)"));
  assert!(text.contains("every procedure declared"), "{text}");
  assert!(text.contains("a@relay mesh on 127.0.0.1:"), "{text}");
  let port: u16 = text
    .lines()
    .find(|l| l.contains("(TCP and UDP)"))
    .and_then(|l| l.split(':').nth(1))
    .and_then(|p| p.split_whitespace().next())
    .and_then(|p| p.parse().ok())
    .unwrap_or_else(|| panic!("no port in:\n{text}"));

  // It serves: /health over TLS on TCP.
  let certificate = NodeCertificate::from_pem_files(&dev.cert, &dev.key).unwrap();
  let client = ReqwestHttpClient::from_config(&http_config(&certificate));
  let health = run(client.execute(HttpRequest::get(format!("https://127.0.0.1:{port}/health"))))
    .await
    .unwrap();
  assert_eq!(health.status, 200);

  // SIGTERM: it stops cleanly.
  let killed = Command::new("kill")
    .args(["-TERM", &child.id().to_string()])
    .status()
    .unwrap();
  assert!(killed.success());
  let status = tokio::task::block_in_place(|| {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
      if let Some(status) = child.try_wait().unwrap() {
        return status;
      }
      assert!(Instant::now() < deadline, "relay did not stop");
      std::thread::sleep(Duration::from_millis(50));
    }
  });
  let text = std::fs::read_to_string(&log).unwrap();
  assert!(status.success(), "{status}:\n{text}");
  assert!(text.contains("stopped"), "{text}");
  let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn relay_binary_refuses_to_start_without_its_settings() {
  let out = Command::new(RELAY)
    .env_clear()
    .output()
    .expect("relay runs");
  assert!(!out.status.success());
  let err = String::from_utf8_lossy(&out.stderr);
  assert!(err.contains("RELAY_MESH_TRUSTED is not set"), "{err}");
}
