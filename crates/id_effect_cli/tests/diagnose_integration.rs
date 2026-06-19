//! Integration tests for the `id-effect-diagnose` binary.

use std::process::Command;

fn diagnose() -> Command {
  Command::new(env!("CARGO_BIN_EXE_id-effect-diagnose"))
}

#[test]
fn example_ok_exits_zero() {
  let out = diagnose().args(["example", "ok"]).output().expect("spawn");
  assert!(
    out.status.success(),
    "stderr={}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(String::from_utf8_lossy(&out.stdout).contains("build_order"));
}

#[test]
fn example_missing_exits_nonzero() {
  let out = diagnose()
    .args(["example", "missing"])
    .output()
    .expect("spawn");
  assert!(!out.status.success());
}

#[test]
fn example_cycle_json() {
  let out = diagnose()
    .args(["--json", "example", "cycle"])
    .output()
    .expect("spawn");
  assert!(!out.status.success());
  assert!(String::from_utf8_lossy(&out.stdout).contains("\"code\""));
}

#[test]
fn providers_subcommand_runs() {
  let out = diagnose().arg("providers").output().expect("spawn");
  assert!(out.status.success());
}

#[test]
fn manifest_json_ok() {
  let dir = std::env::temp_dir();
  let path = dir.join("id_effect_diagnose_cli_ok.json");
  std::fs::write(
    &path,
    r#"{"providers":[
      {"id":"config","provides":"Config"},
      {"id":"db","provides":"Database","requires":["Config"]}
    ]}"#,
  )
  .unwrap();
  let out = diagnose()
    .args(["--json", "manifest", path.to_str().unwrap()])
    .output()
    .expect("spawn");
  assert!(
    out.status.success(),
    "stderr={}",
    String::from_utf8_lossy(&out.stderr)
  );
  let _ = std::fs::remove_file(path);
}

#[test]
fn example_default_missing_exits_nonzero() {
  let out = diagnose().args(["example"]).output().expect("spawn");
  assert!(!out.status.success());
}

#[test]
fn providers_json_output() {
  let out = diagnose()
    .args(["--json", "providers"])
    .output()
    .expect("spawn");
  assert!(out.status.success());
  assert!(String::from_utf8_lossy(&out.stdout).contains("\"ok\""));
}

#[test]
fn manifest_toml_missing_dependency() {
  let dir = std::env::temp_dir();
  let path = dir.join("id_effect_diagnose_cli_bad.toml");
  std::fs::write(
    &path,
    "[[providers]]\nid = \"db\"\nprovides = \"Database\"\nrequires = [\"Config\"]\n",
  )
  .unwrap();
  let out = diagnose()
    .args(["manifest", path.to_str().unwrap()])
    .output()
    .expect("spawn");
  assert!(!out.status.success());
  let _ = std::fs::remove_file(path);
}

#[test]
fn help_exits_zero() {
  let out = diagnose().arg("--help").output().expect("spawn");
  assert!(out.status.success());
  assert!(String::from_utf8_lossy(&out.stdout).contains("id-effect-diagnose"));
}

#[test]
fn manifest_json_cycle_fails() {
  let dir = std::env::temp_dir();
  let path = dir.join("id_effect_diagnose_cli_cycle.json");
  std::fs::write(
    &path,
    r#"{"providers":[
      {"id":"a","provides":"A","requires":["B"]},
      {"id":"b","provides":"B","requires":["A"]}
    ]}"#,
  )
  .unwrap();
  let out = diagnose()
    .args(["--json", "manifest", path.to_str().unwrap()])
    .output()
    .expect("spawn");
  assert!(!out.status.success());
  let _ = std::fs::remove_file(path);
}

#[test]
fn example_ok_json_outputs_success() {
  let out = diagnose()
    .args(["--json", "example", "ok"])
    .output()
    .expect("spawn");
  assert!(out.status.success());
  assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
}

#[test]
fn manifest_missing_file_fails() {
  let out = diagnose()
    .args(["manifest", "/nonexistent/path/manifest.json"])
    .output()
    .expect("spawn");
  assert!(!out.status.success());
}
