# Services: `#[rpc::service]`, the client and the contract

The previous chapters registered procedures one by one on an `RpcGroup`. `#[rpc::service]`
does that from a trait: you write the procedures once, as trait methods, and the macro generates
everything around them — the registration, the Rust client, the contract and the router mount.

```rust
use id_effect::{DynEnv, Effect, Never, Stream, fail, succeed};
use id_effect_rpc as rpc;
use rpc::kinds::Duplex;

#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct Todo {
  id: i64,
  title: String,
}

#[derive(Debug, PartialEq, rpc::RpcErrorUnion)]
enum TodoError {
  #[rpc(status = 404)]
  NotFound { id: i64 },
  #[rpc(status = 409)]
  Locked,
}

/// Keeps todos.
#[rpc::service(version = 1)]
trait Todos {
  /// One todo by id.
  #[query]
  #[public]
  fn get(&self, id: i64) -> Effect<Todo, TodoError, DynEnv>;

  /// 1, 2, …, n.
  #[server_stream]
  #[public]
  fn upto(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;

  /// Every line back in upper case.
  #[bidi]
  #[public]
  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv>;
}

struct Store;

impl Todos for Store {
  fn get(&self, id: i64) -> Effect<Todo, TodoError, DynEnv> {
    match id {
      1 => succeed(Todo { id, title: "milk".into() }),
      2 => fail(TodoError::Locked),
      _ => fail(TodoError::NotFound { id }),
    }
  }

  fn upto(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv> {
    succeed(Stream::from_iterable(1..=n))
  }

  fn shout(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv> {
    succeed(Duplex::new(|lines: Stream<String, Never, DynEnv>| {
      lines.map(|s| s.to_uppercase())
    }))
  }
}

let app: axum::Router = TodosService::server(Store).mount_default().with_state(DynEnv::new());
# let _ = app;
```

## The trait is the handler

The trait stays as you wrote it, minus the macro's attributes: it is the **handler trait**. Each
method returns an `Effect<A, E, DynEnv>`, so a handler reads its capabilities from the
environment, its typed errors are in its signature, and you test it with the test executor like
any other effect — no network, no generated code in the way.

Every method carries exactly two decisions:

- **its kind** — `#[query]`, `#[mutation]`, `#[server_stream]`, `#[client_stream]`, `#[bidi]` or
  `#[subscription]` (chapter 33.1 has the shapes). The kind attribute takes options:
  `#[query(name = "…")]` renames the procedure (the tag is `"<Service>.<name>"`), and
  `max_items = N` / `max_bytes = N` cap a client stream.
- **who may call it** — `#[public]`, or one or more `#[guard(G)]` with any `RouteGuard`
  expression (`id_effect_authz` guards included; the last one is the outermost). There is no
  default: a procedure without a decision does not compile (ADR 0013 R1).

```text
error: procedure `wipe` has no access decision
         help: add `#[public]` to let anyone call it, or `#[guard(G)]` to run a guard first; procedures deny by default (ADR 0013 R1)
 --> src/admin.rs:8:6
  |
8 |   fn wipe(&self, all: bool) -> Effect<(), Never, DynEnv>;
  |      ^^^^
```

The other misuses fail the same way, each first error with a `help:` line: two kinds on one
method, two procedures with one name, a `#[bidi]` method that does not return a `Duplex`, a
missing `version`, a type without a Schema and an error type that is not a union
(`crates/id_effect_rpc/tests/ui`).

## Types and errors

Inputs, outputs and stream items are `String`, `bool`, `i64`, `f64`, `()`, `Option<T>`,
`Vec<T>`, or any `#[derive(id_effect::Schema)]` struct; their JSON Schemas go into the contract.
A method with no parameter takes `()` (`null` on the wire). When a type also implements serde
`Serialize` and `Deserialize` (the built-ins do; add the serde derives next to `Schema`), the
registration attaches its typed codec (`WireSchema::with_serde`), so the procedure is served over
postcard as well as JSON (chapter 33.3); a procedure whose types lack serde stays JSON-only.
Error unions always travel as JSON.

The error type is an enum with `#[derive(RpcErrorUnion)]`, or `Never` when the procedure cannot
fail. Each variant is one member of the union, `{"_tag": "NotFound", "id": 7}` on the wire:

- `#[rpc(status = 404)]` sets the HTTP status of a single call (default 400; on the enum it sets
  every variant's default);
- `#[rpc(tag = "…")]` renames a variant's tag;
- `#[rpc(internal)]` marks a server fault: it is logged and answered as `InternalError { id }`,
  with nothing but the correlation id, and it is left out of the contract.

The reserved failures — `InternalError`, `InvalidInput`, `DeadlineExceeded`, `Cancelled`,
`ResourceExhausted`, `ResumeExpired` — can reach any call, and no variant may take their names.

## What the macro generates

For `trait Todos`, next to the trait:

| Item | What it is |
|---|---|
| `TodosService` | `NAME`, `VERSION`, `contract()`; `register(&mut group, handler)`, `group(handler)`, `server(handler)` (an `RpcServer`), `mount(router, handler)` (the three `/rpc` routes on a typed router); `client(transport)` |
| `TodosClient<T>` | one method per procedure, over any `RpcTransport` `T` |

Client methods return effects whose error is `RpcCallError<E>`: `Typed(E)` for a member of the
union, `Failure(RpcFailure)` for a reserved failure, `Transport(..)` when the call never reached
the procedure (a guard's 403, an unknown version's 404). Streams come back as `Stream`s,
subscription items with the cursor to resume after, and a two-way call as a sender and a stream:

```rust,ignore
use id_effect_rpc::service::LocalTransport;

let client = TodosService::client(LocalTransport::new(TodosService::group(Store), DynEnv::new()));
let todo = client.get(1);                                        // Effect<Todo, RpcCallError<TodoError>, R>
let numbers = client.upto(3).flat_map(|s| s.run_collect());       // [1, 2, 3]
let shouted = client.shout().flat_map(|(tx, lines)| {
  tx.send("hi".to_owned()).map(move |()| drop(tx)).flat_map(move |()| lines.run_collect())
});                                                               // ["HI"]
```

Two transports ship with the macro: `LocalTransport` calls a group in the same process through
the same dispatch as the network (versions, guards, decoding, error mapping), and
`HttpTransport` (feature `client`) sends queries and mutations as `POST /rpc` envelopes through
`id_effect_platform`'s HTTP client. The WebSocket and SSE transports of chapter 33.5 carry the
stream kinds.

## The contract and its snapshot

`TodosService::contract()` is the service's entry in the language-neutral contract (D11): every
procedure's kind, access decision, input/output/error JSON Schemas and doc comment. Check it in,
so a change to the API is a change to a reviewed file:

```rust,ignore
use id_effect_rpc::contract::assert_snapshot;
use id_effect_rpc::service::RpcService;

#[test]
fn contract_snapshot_is_current() {
  assert_snapshot(&TodosService::document(), "rpc.contract.json");
}
```

`UPDATE_GOLDEN=1 cargo test` rewrites the file. `id-effect rpc gen ts` and `id-effect rpc check`
read it without compiling anything.

## Versions side by side

A version is a trait. To change a procedure incompatibly, declare the next version under the same
service name and serve both; a client names its version on every call (`v`), so old clients keep
working:

```rust,ignore
#[rpc::service(version = 1, name = "Greeter")]
trait GreeterV1 { /* … */ }

#[rpc::service(version = 2, name = "Greeter")]
trait GreeterV2 { /* … */ }

let mut group = RpcGroup::new();
GreeterV1Service::register(&mut group, Hello);
GreeterV2Service::register(&mut group, Hello);
```
