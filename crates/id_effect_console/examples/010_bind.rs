//! Prints whether a console bound to each address needs an authorization guard.
//!
//! `cargo run -p id_effect_console --example 010_bind`

use id_effect_console::ConsoleConfig;

fn main() {
  for bind in ["127.0.0.1:9090", "[::1]:9090", "0.0.0.0:9090"] {
    let config = ConsoleConfig::new(bind.parse().expect("socket address"));
    let access = if config.requires_guard() {
      "guard required"
    } else {
      "local only"
    };
    println!("{:<16} {access}", config.bind());
  }
}
