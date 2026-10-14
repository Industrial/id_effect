//! Compile-error UI cases for spike B (real rustc output, committed `.stderr`).
//! Regenerate with `TRYBUILD=overwrite cargo nextest run -p kernel_b --test ui`.

#[test]
fn ui() {
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/*.rs");
}
