use id_effect::{DynEnv, Effect, Never};
use id_effect_rpc as rpc;

#[rpc::service]
trait Greeter {
  #[query]
  #[public]
  fn greet(&self, name: String) -> Effect<String, Never, DynEnv>;
}

fn main() {}
