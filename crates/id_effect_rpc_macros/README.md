# id_effect_rpc_macros

Procedural macros for [`id_effect_rpc`](../id_effect_rpc): `#[rpc::service(version = N)]` turns a
trait of procedures into a typed RPC service — the handler trait (each method an
`Effect<A, E, R>`), the Rust client, the contract and the router mount (ADR 0014).

Use it through `id_effect_rpc`, which re-exports the attribute; depend on this crate directly only
if you write your own RPC front end.

Status (0.12 development): the attribute accepts a trait and emits it unchanged. Procedure kinds
(`#[query]`, `#[mutation]`, `#[server_stream]`, `#[client_stream]`, `#[bidi]`,
`#[subscription]`) and access decisions (`#[public]`, `#[guard(G)]`) arrive later in 0.12.

```rust
use id_effect_rpc_macros::service;

#[service]
trait Greeter {
  fn greet(&self, name: String) -> String;
}

struct Hello;

impl Greeter for Hello {
  fn greet(&self, name: String) -> String {
    format!("hello, {name}")
  }
}

assert_eq!(Hello.greet("rpc".into()), "hello, rpc");
```

Tier `integration` (ADR 0010 Amendment 8). License: MIT.
