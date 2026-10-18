//! `ConsoleConfig` bind addresses and the guard rule.

use std::net::SocketAddr;

use id_effect_console::ConsoleConfig;

fn config(bind: &str) -> ConsoleConfig {
  ConsoleConfig::new(bind.parse::<SocketAddr>().unwrap())
}

#[test]
fn only_non_loopback_binds_require_a_guard() {
  assert_eq!(ConsoleConfig::loopback(9090), config("127.0.0.1:9090"));
  for local in ["127.0.0.1:9090", "127.8.0.1:1", "[::1]:9090"] {
    assert!(!config(local).requires_guard(), "{local}");
  }
  for public in [
    "0.0.0.0:9090",
    "[::]:9090",
    "10.0.0.5:9090",
    "[2001:db8::1]:9090",
  ] {
    assert!(config(public).requires_guard(), "{public}");
  }
}
