//! Compile-fail cases of the raw-SQL capability (M16, ADR 0013 R5): `query` / `execute` reject a
//! run-time `String`, and `query_raw` needs a `RawSqlCapability` nobody can forge.
//!
//! The expected stderr is rustc's, so the cases run only on the toolchain the snapshots were
//! blessed on (`UI_FORCE=1` runs them anywhere; `TRYBUILD=overwrite` re-blesses).

const BLESSED_RUSTC: &str = "rustc 1.99.0 ";

fn on_blessed_rustc(test: &str) -> bool {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  let version = std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default();
  if version.starts_with(BLESSED_RUSTC) || std::env::var_os("UI_FORCE").is_some() {
    return true;
  }
  eprintln!(
    "{test}: skipped on `{}`; snapshots are blessed on `{BLESSED_RUSTC}` (UI_FORCE=1 runs it)",
    version.trim()
  );
  false
}

#[test]
fn m16_query_with_string_does_not_compile() {
  if !on_blessed_rustc("m16_query_with_string_does_not_compile") {
    return;
  }
  trybuild::TestCases::new().compile_fail("tests/ui/m16_query_with_string_does_not_compile.rs");
}

#[test]
fn m16_query_raw_requires_capability() {
  if !on_blessed_rustc("m16_query_raw_requires_capability") {
    return;
  }
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/m16_query_raw_requires_capability.rs");
  t.compile_fail("tests/ui/m16_raw_capability_cannot_be_forged.rs");
}
