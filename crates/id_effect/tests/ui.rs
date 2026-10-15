//! `k-ui-suite`: compile-fail and compile-pass UI tests for the typed `R` of the 0.7 kernel.
//!
//! `fail/` pins the diagnostics of the capability traits (`#[diagnostic::on_unimplemented]` on
//! `Selector`, `Plucker`, `Sculptor`, `Embed`, with `#[diagnostic::do_not_recommend]` on the
//! recursive impls), `caps!`'s duplicate check, the `Send` bound of `effect!` and the
//! documented H17 residue (E0283 for a generic-`R` helper or runner without the pattern). `pass/`
//! pins what must keep compiling: shared `Layer` inputs, the `.same_env()` pattern and a `Send`
//! `effect!` body. The cases were ported from spike A (`spikes/kernel_a/tests/ui`, removed
//! in `k-ui-suite`; see git history at `9cf380e`).
//!
//! # Pinned toolchain
//!
//! rustc's diagnostic wording and type-path trimming change between releases and channels (1.99
//! prints `id_effect::capability::HCons` where 1.97 printed `id_effect::HCons`;
//! nightly words the `Bindable` help block differently). The `.stderr` files are therefore
//! blessed on exactly [`BLESSED_RUSTC`], the version the CI `moon-ci` job pins, and the suite only
//! runs there; on any other compiler it is skipped with a note (`UI_FORCE=1` runs it anyway).
//! When CI's pin moves, re-bless on the new version:
//! `TRYBUILD=overwrite cargo test -p id_effect --test ui` with that rustc first on `PATH`.

/// The exact `rustc -V` prefix the `.stderr` snapshots were blessed with (= CI's pin).
const BLESSED_RUSTC: &str = "rustc 1.99.0 ";

fn rustc_version() -> String {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default()
}

#[test]
fn ui() {
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
