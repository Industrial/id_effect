//! The compile-fail security suite (ADR 0013, E11.4): every `ui/*.rs` misuses a secure-by-default
//! API and must not compile; its `.stderr` pins the diagnostic, `help:` notes included.
//!
//! # Pinned toolchain
//!
//! As in `crates/id_effect_axum/tests/ui.rs`: the `.stderr` files are blessed on
//! [`BLESSED_RUSTC`], the version `rust-toolchain.toml` and CI pin, and the suite only runs there;
//! on any other compiler it is skipped with a note (`UI_FORCE=1` runs it anyway). Re-bless with
//! `TRYBUILD=overwrite cargo test -p security_compile_fail --test compile_fail` when the pin
//! moves.

use std::path::Path;

/// The `rustc -V` prefix the `.stderr` snapshots were blessed with (= CI's pin).
const BLESSED_RUSTC: &str = "rustc 1.99.0";

/// The exit gate of 0.12.0: at least this many cases (0.11.0's 17 and the two rpc cases).
const MIN_CASES: usize = 19;

fn rustc_version() -> String {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default()
}

/// The case names (file stems of `ui/*.rs`), sorted.
fn cases() -> Vec<String> {
  let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
  let mut names: Vec<String> = std::fs::read_dir(&dir)
    .expect("read ui/")
    .map(|entry| entry.expect("ui/ entry").path())
    .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
    .map(|path| {
      path
        .file_stem()
        .expect("case file name")
        .to_string_lossy()
        .into_owned()
    })
    .collect();
  names.sort();
  names
}

#[test]
fn security_misuse_does_not_compile() {
  let version = rustc_version();
  if !version.starts_with(BLESSED_RUSTC) && std::env::var_os("UI_FORCE").is_none() {
    eprintln!(
      "compile_fail: skipped on `{}`; snapshots are blessed on `{BLESSED_RUSTC}` (UI_FORCE=1 runs it)",
      version.trim()
    );
    return;
  }
  let t = trybuild::TestCases::new();
  t.compile_fail("ui/*.rs");
}

#[test]
fn every_case_is_listed_with_its_rule() {
  let cases = cases();
  assert!(
    cases.len() >= MIN_CASES,
    "{} compile-fail cases, the 0.12 gate needs at least {MIN_CASES}",
    cases.len()
  );
  let doc_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/security/compile-fail.md");
  let doc = std::fs::read_to_string(&doc_path).expect("read docs/security/compile-fail.md");
  for case in &cases {
    assert!(
      Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("ui/{case}.stderr"))
        .is_file(),
      "ui/{case}.rs has no .stderr snapshot"
    );
    assert!(
      doc
        .lines()
        .any(|line| line.contains(&format!("`{case}`")) && line.contains("| R")),
      "docs/security/compile-fail.md does not list `{case}` with its ADR 0013 rule"
    );
  }
}
