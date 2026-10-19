//! Compile-fail UI tests of `#[rpc::service]` and `#[derive(RpcErrorUnion)]` (r12-e12-1-macro):
//! each misuse is refused, and its first error carries a `help:` line that states the fix (K4).
//!
//! - `procedure_without_access_decision` — ADR 0013 R1 / ADR 0014 C1 (the security suite's
//!   `r1_rpc_procedure_without_access_decision` is built from this case);
//! - `two_kinds_on_one_method`, `duplicate_procedure_names`, `missing_version`,
//!   `bidi_without_duplex`;
//! - `type_without_schema`, `error_type_not_a_union` — the `on_unimplemented` notes name the
//!   derive;
//! - `reserved_error_tag` — design note §4.
//!
//! # Pinned toolchain
//!
//! As in `crates/id_effect_axum/tests/ui.rs`: the `.stderr` files are blessed on
//! [`BLESSED_RUSTC`] and the suite only runs there (`UI_FORCE=1` runs it anyway). Re-bless with
//! `TRYBUILD=overwrite cargo test -p id_effect_rpc --test ui`.

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
fn rpc_service_misuse_does_not_compile() {
  let version = rustc_version();
  if !version.starts_with(BLESSED_RUSTC) && std::env::var_os("UI_FORCE").is_none() {
    eprintln!(
      "ui: skipped on `{}`; snapshots are blessed on `{BLESSED_RUSTC}` (UI_FORCE=1 runs it)",
      version.trim()
    );
    return;
  }
  trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
