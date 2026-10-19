# id_effect_rpc_macros

Procedural macros for [`id_effect_rpc`](../id_effect_rpc) (ADR 0014):

- `#[rpc::service(version = N)]` turns a trait of procedures into a typed RPC service — the
  handler trait (each method an `Effect<A, E, DynEnv>`), `<Trait>Service` (contract,
  registration, router mount) and `<Trait>Client<T>`, the Rust client over any transport.
- `#[derive(RpcErrorUnion)]` makes an enum a typed error union `{"_tag": …}`, with an HTTP status
  per variant; the reserved failure tags are refused.

Every procedure has one kind (`#[query]`, `#[mutation]`, `#[server_stream]`, `#[client_stream]`,
`#[bidi]`, `#[subscription]`) and one access decision (`#[public]` or `#[guard(G)]`); a procedure
without a decision does not compile (ADR 0013 R1). Use the macros through `id_effect_rpc`, which
re-exports them; depend on this crate directly only if you write your own RPC front end.

```rust
use id_effect::{DynEnv, Effect, fail, succeed};
use id_effect_rpc as rpc;

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
    if name.is_empty() { fail(GreetError::Empty) } else { succeed(format!("Hello, {name}!")) }
  }
}

let contract = GreeterService::contract();
assert_eq!(contract.procedures[0].name, "greet");
assert_eq!(GreeterService::group(Hello).tags().collect::<Vec<_>>(), ["Greeter.greet"]);
```

Book: part 6, "Services: `#[rpc::service]`, the client and the contract". Tier `integration`
(ADR 0010 Amendment 8). License: MIT.
