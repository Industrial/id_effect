//! Ex 000 — the crate builds and links (containers: `010_containers`).
//!
//! Run: `cargo run -p id_effect_test --example 000_crate`

use id_effect_test as _;

fn main() {
  println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
}
