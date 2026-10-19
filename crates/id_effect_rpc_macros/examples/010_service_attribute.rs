//! `#[service]` on a trait: the trait stays usable as written.
//!
//! `cargo run -p id_effect_rpc_macros --example 010_service_attribute`

use id_effect_rpc_macros::service;

#[service]
trait Greeter {
  fn greet(&self, name: String) -> String;
}

struct Hello;

impl Greeter for Hello {
  fn greet(&self, name: String) -> String {
    format!("hello, {name}")
  }
}

fn main() {
  println!("{}", Hello.greet("rpc".into()));
}
