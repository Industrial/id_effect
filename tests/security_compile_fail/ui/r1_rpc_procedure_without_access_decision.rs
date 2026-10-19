// ADR 0013 R1 / ADR 0014 C1: an `#[rpc::service]` procedure must say who may call it
// (`#[public]` or `#[guard(G)]`) before the service compiles; procedures deny by default.
// Built from crates/id_effect_rpc/tests/ui/procedure_without_access_decision.rs.
use id_effect_kit::id_effect::{DynEnv, Effect, Never};
use id_effect_rpc as rpc;

#[rpc::service(version = 1)]
trait Admin {
  #[mutation]
  fn wipe(&self, all: bool) -> Effect<(), Never, DynEnv>;
}

fn main() {}
