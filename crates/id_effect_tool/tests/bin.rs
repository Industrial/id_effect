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
