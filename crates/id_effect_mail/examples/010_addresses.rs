//! Validates a few mail addresses and prints their parts.
//!
//! `cargo run -p id_effect_mail --example 010_addresses`

use id_effect_mail::Address;

fn main() {
  for addr in [
    "carol@ledger.example",
    "no-at-sign",
    "carol@ledger..example",
  ] {
    match Address::parse(addr) {
      Ok(a) => println!("{addr:<24} local={} domain={}", a.local(), a.domain()),
      Err(error) => println!("{addr:<24} rejected: {error}"),
    }
  }
}
