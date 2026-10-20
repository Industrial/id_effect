use id_effect::{DynEnv, Effect, Never};
use id_effect_rpc as rpc;

// No `#[derive(id_effect::Schema)]`.
struct User {
  name: String,
}

#[rpc::service(version = 1)]
trait Users {
  #[query]
  #[public]
  fn me(&self) -> Effect<User, Never, DynEnv>;
}

fn main() {
  let _ = User {
    name: String::new(),
  }
  .name;
}
