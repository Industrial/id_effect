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

/// `.devcontainer/` is plain JSON on a digest-pinned Rust image no older than the workspace MSRV,
/// with Docker for compose services and `id-effect dev` named as the dev loop.
#[test]
fn every_template_ships_a_devcontainer_on_a_pinned_toolchain() {
  let tmp = tempfile::tempdir().unwrap();
  let msrv = minor(env!("CARGO_PKG_RUST_VERSION"));
  let variants: [(&str, &[&str]); 5] = [
    ("service", &["--template", "service"]),
    (
      "service-pg",
      &["--template", "service", "--db", "pg", "--otel"],
    ),
    ("worker", &["--template", "worker"]),
    ("cli", &["--template", "cli"]),
    ("cluster", &["--template", "cluster"]),
  ];
  for (label, args) in variants {
    let dest = tmp.path().join(label);
    let mut argv = vec!["new", "demo", "--dest", dest.to_str().unwrap()];
    argv.extend_from_slice(args);
    let out = id_effect(&argv);
    assert!(out.status.success(), "{label}: {out:?}");
    let text = std::fs::read_to_string(dest.join(".devcontainer/devcontainer.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text)
      .unwrap_or_else(|e| panic!("{label}: devcontainer.json is not JSON ({e}):\n{text}"));
    assert_eq!(json["name"], "demo", "{label}");
    assert_eq!(json["build"]["dockerfile"], "Dockerfile", "{label}");
    assert!(
      json["features"]
        .as_object()
        .is_some_and(|f| f.keys().any(|k| k.contains("/docker-in-docker:"))),
      "{label}: no docker-in-docker feature"
    );
    let post_create = json["postCreateCommand"].as_str().unwrap_or_default();
    assert!(
      post_create.contains("id-effect dev"),
      "{label}: {post_create}"
    );
    let ports = json["forwardPorts"].as_array().cloned().unwrap_or_default();
    if label.starts_with("service") {
      assert_eq!(ports, [serde_json::json!(8080)], "{label}");
    } else {
      assert!(ports.is_empty(), "{label}: {ports:?}");
    }

    let dockerfile = std::fs::read_to_string(dest.join(".devcontainer/Dockerfile")).unwrap();
    let from = dockerfile
      .lines()
      .find_map(|line| line.strip_prefix("FROM "))
      .unwrap_or_else(|| panic!("{label}: no FROM in:\n{dockerfile}"));
    let (image, digest) = from.split_once('@').expect("a digest-pinned image");
    assert!(
      digest.starts_with("sha256:") && digest.len() == 71,
      "{label}: {from}"
    );
    let version = image
      .strip_prefix("rust:")
      .and_then(|tag| tag.split('-').next())
      .unwrap_or_else(|| panic!("{label}: not a rust image: {from}"));
    assert!(
      minor(version) >= msrv,
      "{label}: {version} is older than the MSRV"
    );
    let pin = format!("ID_EFFECT_VERSION={}", env!("CARGO_PKG_VERSION"));
    assert!(dockerfile.contains(&pin), "{label}: no {pin}");
  }
}

/// The minor version of `1.<minor>[.<patch>]`.
fn minor(version: &str) -> u32 {
  let mut parts = version.split('.');
  assert_eq!(parts.next(), Some("1"), "{version}");
  parts.next().and_then(|m| m.parse().ok()).expect(version)
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
