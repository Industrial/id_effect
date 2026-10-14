//! Compile-error UI cases (trybuild). The committed `.stderr` files are real rustc output for the
//! toolchain pinned by devenv; regenerate with `TRYBUILD=overwrite cargo nextest run -p kernel_a
//! --test ui` after a toolchain bump.

#[test]
fn ui() {
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/*.rs");
}
