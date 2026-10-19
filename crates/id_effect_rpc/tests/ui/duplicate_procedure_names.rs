use id_effect::{DynEnv, Effect, Never};
use id_effect_rpc as rpc;

#[rpc::service(version = 1)]
trait Greeter {
  #[query]
  #[public]
  fn greet(&self, name: String) -> Effect<String, Never, DynEnv>;

  #[query(name = "greet")]
  #[public]
  fn greet_twice(&self, name: String) -> Effect<String, Never, DynEnv>;
}

fn main() {}
