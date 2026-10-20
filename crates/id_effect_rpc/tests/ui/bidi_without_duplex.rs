use id_effect::{DynEnv, Effect, Never, Stream};
use id_effect_rpc as rpc;

#[rpc::service(version = 1)]
trait Chat {
  #[bidi]
  #[public]
  fn room(&self) -> Effect<Stream<String, Never, DynEnv>, Never, DynEnv>;
}

fn main() {}
