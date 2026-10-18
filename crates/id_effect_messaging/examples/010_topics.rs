//! Validates a few topic names and prints the result.
//!
//! `cargo run -p id_effect_messaging --example 010_topics`

use id_effect_messaging::Topic;

fn main() {
  for name in ["ledger.entry-posted", "orders_v2", "", "user signed up"] {
    match Topic::new(name) {
      Ok(topic) => println!("{topic:<22} ok"),
      Err(error) => println!("{name:<22} rejected: {error}"),
    }
  }
}
