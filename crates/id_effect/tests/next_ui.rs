//! `k-ui-suite`: compile-fail and compile-pass UI tests for the `id_effect::next` typed `R`.
//!
//! `fail/` pins the diagnostics of the capability traits (`#[diagnostic::on_unimplemented]` on
//! `Selector`, `Plucker`, `Sculptor`, `Embed`, with `#[diagnostic::do_not_recommend]` on the
//! recursive impls), `caps_next!`'s duplicate check, the `Send` bound of `effect_next!` and the
//! documented H17 residue (E0283 for a generic-`R` helper or runner without the pattern). `pass/`
//! pins what must keep compiling: shared `Layer` inputs, the `.same_env()` pattern and a `Send`
//! `effect_next!` body. The cases were ported from spike A (`spikes/kernel_a/tests/ui`, removed
//! in `k-ui-suite`; see git history at `9cf380e`).
//!
//! The `.stderr` files are generated on the stable toolchain the CI uses:
//! `TRYBUILD=overwrite PATH="$RUST_STABLE_BIN:$PATH" cargo test -p id_effect --test next_ui`.
//!
//! `missing_capability_narrowed` is stable-only: its trailing `Bindable` help block is worded
//! differently on nightly ("conditionally implemented", "unsatisfied requirement introduced
//! here"), while the custom `Embed` message it pins is identical. The other cases agree on both.

/// Cases whose `.stderr` depends on the toolchain channel; compiled only on stable.
const STABLE_ONLY: &[&str] = &["missing_capability_narrowed.rs"];

fn on_nightly() -> bool {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).contains("nightly"))
    .unwrap_or(false)
}

#[test]
fn next_ui() {
  let t = trybuild::TestCases::new();
  let nightly = on_nightly();
  let mut fail: Vec<_> = std::fs::read_dir("tests/next_ui/fail")
    .expect("tests/next_ui/fail")
    .map(|e| e.expect("dir entry").path())
    .filter(|p| p.extension().is_some_and(|x| x == "rs"))
    .filter(|p| !(nightly && STABLE_ONLY.iter().any(|s| p.ends_with(s))))
    .collect();
  fail.sort();
  for case in fail {
    t.compile_fail(case);
  }
  t.pass("tests/next_ui/pass/*.rs");
}
