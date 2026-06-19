//! Integration tests for the `id-effect` binary.

use std::process::Command;

fn id_effect() -> Command {
  Command::new(env!("CARGO_BIN_EXE_id-effect"))
}

#[test]
fn version_subcommand_prints_version() {
  let out = id_effect().arg("version").output().expect("spawn");
  assert!(out.status.success());
  let stdout = String::from_utf8_lossy(&out.stdout);
  assert!(stdout.contains("id-effect"));
}

#[test]
fn new_subcommand_scaffolds_temp_project() {
  let dest = std::env::temp_dir().join(format!("id_effect_cli_new_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dest);
  let out = id_effect()
    .args([
      "new",
      "tmp-cli-app",
      "--dest",
      dest.to_str().unwrap(),
      "--id-effect-path",
      "../id_effect",
      "--id-effect-cli-path",
      "../id_effect_cli",
    ])
    .output()
    .expect("spawn");
  assert!(
    out.status.success(),
    "stderr={}",
    String::from_utf8_lossy(&out.stderr)
  );
  assert!(dest.join("src/main.rs").exists());
  let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn new_existing_nonempty_dest_fails() {
  let dest = std::env::temp_dir().join(format!("id_effect_cli_dup_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dest);
  std::fs::create_dir_all(&dest).unwrap();
  std::fs::write(dest.join("occupied.txt"), "x").unwrap();
  let out = id_effect()
    .args(["new", "dup-app", "--dest", dest.to_str().unwrap()])
    .output()
    .expect("spawn");
  assert!(!out.status.success());
  let _ = std::fs::remove_dir_all(&dest);
}
