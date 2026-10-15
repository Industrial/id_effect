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

#[test]
fn next_ui() {
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/next_ui/fail/*.rs");
  t.pass("tests/next_ui/pass/*.rs");
}
