# `id_effect_rpc`

RPC client/server stack and HTTP boundary helpers for [`id_effect`](../id_effect) — [`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) parity (Phase D).

## Full stack (D3)

| Module | Role |
|--------|------|
| [`protocol`](src/protocol.rs) | Tagged wire request/response (`POST /rpc`) |
| [`serialization`](src/serialization.rs) | JSON + [`id_effect::schema`] encode/decode |
| [`registry`](src/registry.rs) | [`RpcGroup`] — register typed handlers |
| [`server`](src/server.rs) | [`RpcServer`] — Axum dispatch route |
| [`client`](src/client.rs) | [`RpcClient`] — remote calls via [`HttpClientKey`] |
| [`stream`](src/stream.rs) | NDJSON stream chunk encoding |

## Edge helpers (D2)

- **`RpcError` / `RpcEnvelope`** — JSON errors + Axum `IntoResponse`
- **`correlation`** — `x-correlation-id`
- **`span`** — `rpc.request` tracing fields
- **`versioning`** — API version negotiation middleware
- **`openapi` / `codegen`** — OpenAPI 3 emission + trait stub strings

## Quick start

```rust
use std::sync::Arc;
use id_effect::{Env, schema, succeed};
use id_effect_rpc::{RpcClient, RpcGroup, RpcServer};

let mut group = RpcGroup::new();
group.register(
  "greet",
  Arc::new(schema::struct_("name", schema::string::<()>(), "enthusiasm", schema::i64::<>())),
  &["name", "enthusiasm"],
  Arc::new(schema::struct_("message", schema::string::<()>(), "count", schema::i64::<>())),
  &["message", "count"],
  |(name, n), _| succeed((format!("Hello, {name}!"), n)),
);
let app = RpcServer::new(group).mount_default().with_state(Env::new());
```

Wire format:

```json
{"tag":"greet","payload":{"name":"Ada","enthusiasm":3}}
{"kind":"success","tag":"greet","success":{"message":"Hello, Ada!","count":3}}
```

## Examples

```bash
cargo run -p id_effect_rpc --example 010_json_greet
cargo run -p id_effect_rpc --example 020_rpc_tracing
```

## Phase D status

- **D1–D2:** patterns, mdBook, edge helpers — shipped
- **D3:** tagged dispatch, `RpcGroup`, `RpcClient`, schema bridging — shipped (HTTP+JSON)
- **Backlog:** proc-macro codegen, gRPC/tonic protocol layers, bidirectional streaming

See `docs/effect-ts-parity/phases/phase-d-rpc.md`.
