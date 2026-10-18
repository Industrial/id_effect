//! Validates a few object keys and prints their file names.
//!
//! `cargo run -p id_effect_storage --example 010_object_keys`

use id_effect_storage::ObjectKey;

fn main() {
  for key in [
    "invoices/2026/inv-0042.pdf",
    "avatars//carol.png",
    "../etc/passwd",
  ] {
    match ObjectKey::new(key) {
      Ok(k) => println!("{key:<28} file_name={}", k.file_name()),
      Err(error) => println!("{key:<28} rejected: {error}"),
    }
  }
}
