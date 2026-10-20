//! Compile-fail and compile-pass UI tests for typed routes (H8, ADR 0013 R1).
//!
//! `fail/` pins that a route without an access decision does not compile (the
//! `#[diagnostic::on_unimplemented]` message of `route::Decided`) and that the `route_unchecked`
//! shim is deprecated; `pass/` pins what must keep compiling: public routes, guarded routes and
//! decided plain endpoints.
//!
//! # Pinned toolchain
//!
//! As in `crates/id_effect/tests/ui.rs`: the `.stderr` files are blessed on [`BLESSED_RUSTC`], the
//! version `rust-toolchain.toml` and CI pin, and the suite only runs there; on any other compiler
//! it is skipped with a note (`UI_FORCE=1` runs it anyway). Re-bless with
//! `TRYBUILD=overwrite cargo test -p id_effect_axum --test ui` when the pin moves.

/// The `rustc -V` prefix the `.stderr` snapshots were blessed with (= CI's pin).
const BLESSED_RUSTC: &str = "rustc 1.99.0";

fn rustc_version() -> String {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default()
}

#[test]
fn h8_route_without_access_decision_does_not_compile() {
  let version = rustc_version();
  if !version.starts_with(BLESSED_RUSTC) && std::env::var_os("UI_FORCE").is_none() {
    eprintln!(
      "ui: skipped on `{}`; snapshots are blessed on `{BLESSED_RUSTC}` (UI_FORCE=1 runs it)",
      version.trim()
    );
    return;
  }
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/fail/*.rs");
  t.pass("tests/ui/pass/*.rs");
}
