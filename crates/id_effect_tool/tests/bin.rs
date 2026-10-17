//! The installed `id-effect` binary end to end: the templates travel inside it.

use std::process::{Command, Output};

fn id_effect(args: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_id-effect"))
    .args(args)
    .output()
    .expect("spawn id-effect")
}

#[test]
fn new_generates_every_template_from_any_working_directory() {
  let tmp = tempfile::tempdir().unwrap();
  for template in ["service", "worker", "cli", "cluster"] {
    let dest = tmp.path().join(template);
    let out = Command::new(env!("CARGO_BIN_EXE_id-effect"))
      .current_dir(tmp.path())
      .args(["new", "demo", "--template", template, "--dest"])
      .arg(&dest)
      .output()
      .expect("spawn id-effect");
    assert!(out.status.success(), "{template}: {out:?}");
    for file in [
      "Cargo.toml",
      "Dockerfile",
      ".github/workflows/ci.yml",
      "src/main.rs",
    ] {
      assert!(dest.join(file).is_file(), "{template}: no {file}");
    }
  }
}

/// Rolling deploys: Kamal's proxy gates on `/ready` and waits longer than the service drains.
#[test]
fn the_service_deploy_config_checks_ready_and_outwaits_the_drain() {
  let tmp = tempfile::tempdir().unwrap();
  let dest = tmp.path().join("service");
  let dest_text = dest.to_str().unwrap();
  let out = id_effect(&["new", "demo", "--template", "service", "--dest", dest_text]);
  assert!(out.status.success(), "{out:?}");
  let deploy = std::fs::read_to_string(dest.join("config/deploy.yml")).unwrap();
  let value = |key: &str| {
    deploy
      .lines()
      .find_map(|line| line.trim_start().strip_prefix(key))
      .unwrap_or_else(|| panic!("no {key} in deploy.yml:\n{deploy}"))
      .trim()
      .trim_matches('"')
      .to_owned()
  };
  let proxy = deploy.split("\nproxy:\n").nth(1).expect("a proxy section");
  let healthcheck = proxy
    .split("healthcheck:\n")
    .nth(1)
    .expect("a proxy healthcheck");
  assert_eq!(
    healthcheck.lines().next().map(str::trim),
    Some("path: /ready"),
    "{deploy}"
  );
  let drain: u64 = value("drain_timeout:").parse().unwrap();
  let grace: u64 = value("SHUTDOWN_TIMEOUT_SECS:").parse().unwrap();
  assert_eq!((drain, grace), (35, 30), "{deploy}");
  assert!(
    drain > grace,
    "drain_timeout {drain} <= shutdown grace {grace}"
  );
}

#[test]
fn a_non_empty_destination_exits_1() {
  let tmp = tempfile::tempdir().unwrap();
  std::fs::write(tmp.path().join("README.md"), "mine").unwrap();
  let dest = tmp.path().to_str().unwrap();
  let out = id_effect(&["new", "demo", "--template", "cli", "--dest", dest]);
  assert_eq!(out.status.code(), Some(1), "{out:?}");
  assert!(String::from_utf8_lossy(&out.stderr).contains("already exists and is not empty"));
}

#[test]
fn a_missing_template_is_a_usage_error() {
  let out = id_effect(&["new", "demo"]);
  assert_eq!(out.status.code(), Some(2), "{out:?}");
}
