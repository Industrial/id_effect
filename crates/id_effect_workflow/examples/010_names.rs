//! Validates a few workflow names and prints the result.
//!
//! `cargo run -p id_effect_workflow --example 010_names`

use id_effect_workflow::WorkflowName;

fn main() {
  for name in ["close-account", "orders.v2", "", "close account"] {
    match WorkflowName::new(name) {
      Ok(name) => println!("{name:<16} ok"),
      Err(error) => println!("{name:<16} rejected: {error}"),
    }
  }
}
