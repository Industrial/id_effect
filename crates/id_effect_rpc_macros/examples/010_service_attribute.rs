//! `#[rpc::service]` on a trait: the contract, a group serving an implementation, and the
//! generated client calling it in-process.
//!
//! `cargo run -p id_effect_rpc_macros --example 010_service_attribute`

use id_effect::{DynEnv, Effect, HNil, fail, succeed};
use id_effect_rpc as rpc;
use id_effect_rpc::service::LocalTransport;

#[derive(Debug, rpc::RpcErrorUnion)]
enum GreetError {
  #[rpc(status = 422)]
  Empty,
}

/// Says hello.
#[rpc::service(version = 1)]
trait Greeter {
  /// Greets someone.
  #[query]
  #[public]
  fn greet(&self, name: String) -> Effect<String, GreetError, DynEnv>;
}

struct Hello;

impl Greeter for Hello {
  fn greet(&self, name: String) -> Effect<String, GreetError, DynEnv> {
    if name.is_empty() {
      fail(GreetError::Empty)
    } else {
      succeed(format!("Hello, {name}!"))
    }
  }
}

#[tokio::main]
async fn main() {
  println!(
    "{}",
    rpc::Contract::new()
      .with_service(GreeterService::contract())
      .to_json_pretty()
  );
  let client = GreeterService::client(LocalTransport::new(
    GreeterService::group(Hello),
    DynEnv::new(),
  ));
  let hello = id_effect_axum::run_with_env(HNil, move |_| client.greet("rpc".into())).await;
  println!("{:?}", hello.into_result().ok());
}
