// ADR 0013 R1: a procedure without `#[public]` or `#[guard(G)]` does not compile.
use id_effect::{DynEnv, Effect, Never};
use id_effect_rpc as rpc;

#[rpc::service(version = 1)]
trait Admin {
  #[mutation]
  fn wipe(&self, all: bool) -> Effect<(), Never, DynEnv>;
}

fn main() {}
