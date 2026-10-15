# RPC boundaries with id_effect (`@effect/rpc` parity)

Effect.ts [`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) ties **Schema**, **Layer**,
and **HTTP** so typed contracts cross process boundaries. In Rust there is no single blessed stack;
this chapter documents **patterns** and the labs crate **`id_effect_rpc`** (`labs/id_effect_rpc`,
unpublished) for **RPC-shaped HTTP** on top of **`id_effect_axum`** and **`id_effect::schema`**.

## Choosing a wire stack (tonic, tarpc, HTTP+JSON)

| Approach | Strengths | Trade-offs |
|----------|-----------|------------|
| **[`tonic`](https://docs.rs/tonic)** + **Protobuf** | Mature gRPC ecosystem, streaming, codegen from `.proto` | Separate schema language; protobuf ↔ `Effect` error mapping is manual at generated boundaries |
| **[`tarpc`](https://docs.rs/tarpc)** | Rust-native service traits, pluggable transports | Fewer batteries than tonic for cross-language clients; still need an explicit error wire type |
| **HTTP + JSON + `Schema`** | Same `Schema` as domain validation; easy to debug; fits Axum | No streaming parity on day one; discipline required for versioning and error envelopes |

**Recommendation for `id_effect` today:** use **HTTP + JSON** for public APIs where you already host
**Axum**, validate bodies with **`Schema::decode_unknown`** via
[`id_effect_axum::json::decode_json_schema`](./ch07-08-axum-host.md), and return structured failures
with **`id_effect_rpc::RpcError`**. Add **gRPC (`tonic`)** when you need IDL-first codegen or
cross-language streaming at scale.

## Effect, `R`, and errors at the edge

- Keep **`Effect<A, E, R>`** for domain logic; **`E`** stays rich inside the process.
- At the Axum route, **map** `E` (and schema failures) into **`RpcError`** or **422** JSON from
  `decode_json_schema` — clients see a **stable wire contract**, not internal enums.
- Propagate **`x-correlation-id`** (read or generate with **`id_effect_rpc::correlation`**) and
  attach it to **responses** so logs and traces line up across hops.

## Tracing and OpenTelemetry

Use **`id_effect_rpc::span::rpc_request_span`** for a stable **`rpc.request`** span name and fields
(`http.method`, `http.route`, `rpc.operation`, `correlation.id`). When your binary installs a
subscriber with an **OpenTelemetry** layer ([OpenTelemetry](./ch07-12-opentelemetry.md)), these
fields map cleanly to semantic conventions without `id_effect_rpc` depending on OTEL crates.

## Labs crate: `id_effect_rpc`

| Piece | Role |
|-------|------|
| **`RpcError` / `RpcEnvelope`** | JSON body + HTTP status + `IntoResponse` for Axum |
| **`RpcGroup` / `RpcServer`** | typed handlers `Fn(A, &mut DynEnv) -> Effect<B, RpcError, DynEnv>`, dispatched at `POST /rpc` on a `Router<DynEnv>` |
| **`RpcClient`** | `call(..)` → an `Effect` over any `R` containing `HttpClientService` |
| **`correlation`** | `x-correlation-id` read, generate, append |
| **`span`** | `tracing` helpers for RPC-shaped requests |
| **`codegen`, `openapi`, `versioning`, `stream`** | trait stubs, OpenAPI emission, version negotiation, NDJSON streams |

Each handler's effect runs as one request fiber (`id_effect_axum::run_with_env`). A typed
`RpcError` becomes a failure envelope; a defect or interrupt becomes `RpcError::internal` (HTTP 500,
the panic message is not sent).

```rust
use id_effect::{DynEnv, succeed};
use id_effect_platform::http::{ReqwestHttpClient, http_client_env};

group.register("greet", payload, &["name", "n"], success, &["message", "n"],
    |(name, n), _env: &mut DynEnv| succeed((format!("Hello, {name}!"), n)));
let app = RpcServer::new(group).mount_default().with_state(DynEnv::new());

let out = client
    .call("greet", input, &p, &pf, &s, &sf)
    .provide_all(http_client_env(ReqwestHttpClient::default_client()))
    .block_on() // or TokioRuntime::current()?.run(..).await in async code
    .into_result()?;
```

API index: `cargo doc --manifest-path labs/Cargo.toml -p id_effect_rpc --open`.

## Example: Axum + Schema + `RpcError`

```bash
cargo run --manifest-path labs/Cargo.toml -p id_effect_rpc --features examples --example 010_json_greet
```

It accepts `POST /greet` with JSON `{"name": string, "enthusiasm": i64}`, validates with
**`Schema`**, rejects bad enthusiasm with **`RpcError::invalid_argument`**, and echoes
**`x-correlation-id`**. `020_rpc_tracing` adds the span helpers.

## Further reading

- [Axum host](./ch07-08-axum-host.md) — `id_effect_axum`, `execute`, JSON + schema
- [Schema](../part4/ch14-00-schema.md) — wire types and `ParseError`
- Phase spec: `docs/effect-ts-parity/phases/phase-d-rpc.md`
