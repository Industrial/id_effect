//! `id-effect doctor` end to end, on the fixture projects under `tests/fixtures/doctor/` and on
//! a freshly generated service.

use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("tests/fixtures/doctor")
    .join(name)
}

fn doctor(args: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_id-effect"))
    .arg("doctor")
    .args(args)
    // The checks read these; keep the host's values out of the runs.
    .env_remove("DATABASE_URL")
    .env_remove("PORT")
    .env_remove("HOST")
    .output()
    .expect("spawn id-effect")
}

#[test]
fn mixed_id_effect_versions_fail_the_versions_check() {
  let dir = fixture("mixed-versions");
  let out = doctor(&["--path", dir.to_str().unwrap()]);
  let stdout = String::from_utf8_lossy(&out.stdout);
  assert_eq!(out.status.code(), Some(1), "{stdout}");
  let line = stdout
    .lines()
    .find(|l| l.contains(" versions "))
    .expect("a versions line");
  assert!(line.trim_start().starts_with("fail"), "{line}");
  assert!(
    line.contains("0.6.2 (id_effect, id_effect_axum), 0.7.0 (id_effect)"),
    "{line}"
  );
  assert!(
    stdout.contains("  pass  project   package `mixed-versions`"),
    "{stdout}"
  );
}

#[test]
fn json_reports_every_check_with_a_status_and_reason() {
  let dir = fixture("mixed-versions");
  let out = doctor(&["--json", "--path", dir.to_str().unwrap()]);
  assert_eq!(out.status.code(), Some(1));
  let json = String::from_utf8(out.stdout).unwrap();
  assert!(json.starts_with('{') && json.ends_with("}\n"), "{json}");
  assert!(json.contains("\"ok\":false"), "{json}");
  let versions = "{\"name\":\"versions\",\"status\":\"fail\",\"reason\":\"id_effect crates resolve to 2 versions";
  assert!(json.contains(versions), "{json}");
  for name in ["project", "rustc", "env", "database", "docker", "port"] {
    assert!(
      json.contains(&format!("\"name\":\"{name}\"")),
      "{name}: {json}"
    );
  }
  // No .env.example, DATABASE_URL, Dockerfile or PORT: those checks do not claim a pass.
  for name in ["env", "database", "docker", "port"] {
    let skipped = format!("\"name\":\"{name}\",\"status\":\"skip\"");
    assert!(json.contains(&skipped), "{name}: {json}");
  }
}

#[test]
fn a_generated_service_passes_once_its_environment_is_set() {
  let tmp = tempfile::tempdir().unwrap();
  let dest = tmp.path().join("svc");
  let new = Command::new(env!("CARGO_BIN_EXE_id-effect"))
    .args(["new", "svc", "--template", "service", "--dest"])
    .arg(&dest)
    .output()
    .unwrap();
  assert!(new.status.success(), "{new:?}");
  let path = dest.to_str().unwrap();
  // Before `.env` exists the keys of .env.example are unset.
  let before = doctor(&["--path", path]);
  let stdout = String::from_utf8_lossy(&before.stdout);
  assert_eq!(before.status.code(), Some(1), "{stdout}");
  assert!(stdout.contains("  fail  env "), "{stdout}");
  // Copied, with a free port: no check fails (versions warns: there is no Cargo.lock yet).
  let port = std::net::TcpListener::bind("127.0.0.1:0")
    .unwrap()
    .local_addr()
    .unwrap()
    .port();
  let example = std::fs::read_to_string(dest.join(".env.example")).unwrap();
  let env = example.replace("PORT=8080", &format!("PORT={port}"));
  std::fs::write(dest.join(".env"), env).unwrap();
  let after = doctor(&["--path", path]);
  let stdout = String::from_utf8_lossy(&after.stdout);
  assert_eq!(after.status.code(), Some(0), "{stdout}");
  assert!(stdout.contains("  pass  env "), "{stdout}");
  assert!(stdout.contains("  pass  port "), "{stdout}");
}
