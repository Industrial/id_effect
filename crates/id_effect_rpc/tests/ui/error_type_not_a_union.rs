use id_effect::{DynEnv, Effect};
use id_effect_rpc as rpc;

// No `#[derive(rpc::RpcErrorUnion)]`.
#[derive(Debug)]
enum TodoError {
  NotFound,
}

#[rpc::service(version = 1)]
trait Todos {
  #[query]
  #[public]
  fn get(&self, id: i64) -> Effect<String, TodoError, DynEnv>;
}

fn main() {
  let _ = TodoError::NotFound;
}
