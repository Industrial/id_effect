//! Ex 000 — the crate builds and links. Real examples arrive with its 0.9 API.
//!
//! Run: `cargo run -p id_effect_auth --example 000_crate`

use id_effect_auth as _;

fn main() {
  println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
}
