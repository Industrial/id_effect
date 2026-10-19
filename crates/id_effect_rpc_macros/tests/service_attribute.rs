//! `#[service]` keeps the annotated trait usable.

use id_effect_rpc_macros::service;

#[service(version = 1)]
trait Greeter {
  fn greet(&self, name: String) -> String;
}

struct Hello;

impl Greeter for Hello {
  fn greet(&self, name: String) -> String {
    format!("hello, {name}")
  }
}

#[test]
fn the_annotated_trait_is_implementable() {
  assert_eq!(Hello.greet("rpc".into()), "hello, rpc");
}
