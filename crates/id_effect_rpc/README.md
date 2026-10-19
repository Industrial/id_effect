# `id_effect_rpc`

Typed RPC for [`id_effect`](../id_effect): the first-party call surface of
[ADR 0014](../../docs/adrs/0014-one-call-surface.md) ([`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc)
parity). Tier `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md) Amendment 8).

A service is a set of tagged procedures. Each has a kind (query, mutation, server stream, client
stream, two-way stream, subscription), input and output schemas, typed errors, and an **access
decision** — `public()` or `guard(g)` — without which it does not compile. A serde
[`contract`](src/contract.rs) describes the services for generated clients and `id-effect rpc check`.

## Quick start

```rust
use std::sync::Arc;
use id_effect::{DynEnv, schema, succeed};
use id_effect_rpc::{Procedure, RpcGroup, RpcServer};

let mut group = RpcGroup::new();
group.register(
  Procedure::query("Greeter.greet").public(),
  Arc::new(schema::struct_("name", schema::string::<()>(), "enthusiasm", schema::i64::<()>())),
  Arc::new(schema::struct_("message", schema::string::<()>(), "count", schema::i64::<()>())),
  &["message", "count"],
  |(name, n), _env| succeed((format!("Hello, {name}!"), n)),
);
let contract = group.service_contract("Greeter", 1);
let app: axum::Router = RpcServer::new(group).mount_default().with_state(DynEnv::new());
```

Wire format of `POST /rpc` (the 0.12 envelope of [`wire`](src/wire.rs) replaces it as the call
kinds land):

```json
{"tag":"Greeter.greet","payload":{"name":"Ada","enthusiasm":3}}
{"kind":"success","tag":"Greeter.greet","success":{"message":"Hello, Ada!","count":3}}
```

An unknown tag answers `404` `application/problem+json`; a guard's refusal is the guard's response.

## Modules

| Module | Role |
|--------|------|
| [`contract`](src/contract.rs) | Service contract model (serde), `assert_snapshot` golden for `rpc.contract.json` |
| [`wire`](src/wire.rs) | Call envelope, exit union, reserved failures, frames, cursors, resume sessions |
| [`codec`](src/codec/mod.rs) | `Codec` trait, JSON, content-type and subprotocol negotiation |
| [`registry`](src/registry.rs) | `Procedure` (access typestate) and `RpcGroup` |
| [`server`](src/server/mod.rs) | `RpcServer`: `/rpc` as a declared typed route, transport-independent `dispatch` |
| [`client`](src/client/mod.rs) | `RpcClient` over `HttpClientService` (feature `client`, default) |
| `protocol`, `serialization`, `stream` | Tagged JSON wire format, Schema bridging, NDJSON chunks |
| `error`, `correlation`, `span`, `versioning`, `openapi` | Edge helpers: `RpcError`, `x-correlation-id`, tracing, API versions, OpenAPI 3.0 |

## Features

Declared once for the 0.12 release: `client` (default), `postcard`, `ws`, `sse`, `events`, `mesh`,
`channels`, `http3`, `webtransport`, `test-util`. The design note
`.maestro/specs/releases/0.12/r12-rpc-design.md` fixes the wire, frame, cursor and contract formats
they implement.

## Examples

```bash
cargo run -p id_effect_rpc --example 010_json_greet
cargo run -p id_effect_rpc --example 020_rpc_tracing
```

Book: [One call surface](../id_effect/book/src/part6/ch33-00-one-call-surface.md),
[API boundaries](../id_effect/book/src/part6/ch29-00-api-boundaries.md).
