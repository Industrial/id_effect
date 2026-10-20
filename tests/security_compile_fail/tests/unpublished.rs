//! The security test crate is never published (ADR 0010 Amendment 7).

#[test]
fn manifest_is_publish_false() {
  let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
    .expect("read Cargo.toml");
  assert!(
    manifest
      .lines()
      .any(|line| line.trim() == "publish = false"),
    "tests/security_compile_fail must stay publish = false"
  );
}
