//! The crate builds and links as a dependency.

use id_effect_test as _;

#[test]
fn crate_links_at_the_workspace_version() {
  assert_eq!(env!("CARGO_PKG_NAME"), "id_effect_test");
  assert!(!env!("CARGO_PKG_VERSION").is_empty());
}
