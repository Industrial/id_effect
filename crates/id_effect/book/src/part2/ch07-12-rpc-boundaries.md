# RPC boundaries with id_effect (`@effect/rpc` parity)

Effect.ts [`@effect/rpc`](https://effect-ts.github.io/effect/docs/rpc) ties **Schema**, **Layer**,
and **HTTP** so typed contracts cross process boundaries. In Rust there is no single blessed stack;
this chapter documents **patterns** and the crate **`id_effect_rpc`** (`crates/id_effect_rpc`, tier
`integration` since 0.12) for **RPC-shaped HTTP** on top of **`id_effect_axum`** and
**`id_effect::schema`**. Since 0.12 RPC is id_effect's first-party call surface (ADR 0014); the
whole picture — procedure kinds, the contract, transports — is
[One call surface](../part6/ch33-00-one-call-surface.md).

## Choosing a wire stack (tonic, tarpc, HTTP+JSON)

| Approach | Strengths | Trade-offs |
|----------|-----------|------------|
| **[`tonic`](https://docs.rs/tonic)** + **Protobuf** | Mature gRPC ecosystem, streaming, codegen from `.proto` | Separate schema language; protobuf ↔ `Effect` error mapping is manual at generated boundaries |
| **[`tarpc`](https://docs.rs/tarpc)** | Rust-native service traits, pluggable transports | Fewer batteries than tonic for cross-language clients; still need an explicit error wire type |
| **HTTP + JSON + `Schema`** | Same `Schema` as domain validation; easy to debug; fits Axum | No streaming parity on day one; discipline required for versioning and error envelopes |

**Recommendation for `id_effect` today:** use **HTTP + JSON** for public APIs where you already host
**Axum**, declare and validate request input with the **typed routes** of
[`id_effect_axum::route`](../part6/ch29-01-typed-routes.md) (or its `ValidJson` extractor), which
answer invalid input with an RFC 9457 `Problem`, and return structured failures with
**`id_effect_rpc::RpcError`**. Add **gRPC (`tonic`)** when you need IDL-first codegen or
cross-language streaming at scale.

## Effect, `R`, and errors at the edge

- Keep **`Effect<A, E, R>`** for domain logic; **`E`** stays rich inside the process.
- At the Axum route, **map** `E` into **`RpcError`** or a `route::Problem`; schema failures are
  already a **422** `application/problem+json` from the typed route or extractor — clients see a
  **stable wire contract**, not internal enums.
- Propagate **`x-correlation-id`** (read or generate with **`id_effect_rpc::correlation`**) and
  attach it to **responses** so logs and traces line up across hops.

## Tracing and OpenTelemetry

Use **`id_effect_rpc::span::rpc_request_span`** for a stable **`rpc.request`** span name and fields
(`http.method`, `http.route`, `rpc.operation`, `correlation.id`). When your binary installs a
subscriber with an **OpenTelemetry** layer ([OpenTelemetry](./ch07-12-opentelemetry.md)), these
fields map cleanly to semantic conventions without `id_effect_rpc` depending on OTEL crates.

## The crate: `id_effect_rpc`

| Piece | Role |
|-------|------|
| **`Procedure` / `RpcGroup`** | tagged procedures, each `public()` or `guard(g)` (an undecided one does not compile); handlers `Fn(A, &mut DynEnv) -> Effect<B, RpcError, DynEnv>` |
| **`RpcServer`** | `POST /rpc` as a declared typed route of `id_effect_axum::route::Router`; per-call guards; unknown tag → 404 Problem |
| **`RpcClient`** | `call(..)` → an `Effect` over any `R` containing `HttpClientService` (feature `client`) |
| **`contract`, `wire`, `codec`** | the serde service contract, the 0.12 wire envelope and frames, JSON codec and negotiation |
| **`RpcError` / `RpcEnvelope`** | JSON body + HTTP status + `IntoResponse` for Axum |
| **`correlation`, `span`** | `x-correlation-id` and `tracing` helpers for RPC-shaped requests |
| **`openapi`, `versioning`, `stream`** | OpenAPI 3.0 emission, version negotiation, NDJSON stream chunks |

Each handler's effect runs as one request fiber (`id_effect_axum::run_with_env`). A typed
`RpcError` becomes a failure envelope; a defect or interrupt becomes `RpcError::internal` (HTTP 500,
the panic message is not sent).

<!-- ignore: id_effect_rpc is not a book_snippets dependency; fragment of the crate's own doc examples -->
```rust,ignore
use id_effect::{DynEnv, succeed};
use id_effect_platform::http::{ReqwestHttpClient, http_client_env};
use id_effect_rpc::{Procedure, RpcClient, RpcGroup, RpcServer};

let mut group = RpcGroup::new();
group.register(Procedure::query("Greeter.greet").public(), payload, success, &["message", "n"],
    |(name, n), _env: &mut DynEnv| succeed((format!("Hello, {name}!"), n)));
let app = RpcServer::new(group).mount_default().with_state(DynEnv::new());

let out = RpcClient::new("http://127.0.0.1:8080")
    .call("Greeter.greet", input, &payload_schema, &["name", "n"], &success_schema)
    .provide_all(http_client_env(ReqwestHttpClient::default_client()))
    .block_on() // or TokioRuntime::current()?.run(..).await in async code
    .into_result()?;
```

API index: `cargo doc -p id_effect_rpc --open`.

## Example: Axum + Schema + `RpcError`

```bash
cargo run -p id_effect_rpc --example 010_json_greet
```

It accepts `POST /greet` with JSON `{"name": string, "enthusiasm": i64}`, validates with
**`Schema`**, rejects bad enthusiasm with **`RpcError::invalid_argument`**, and echoes
**`x-correlation-id`**. `020_rpc_tracing` adds the span helpers.

## Further reading

- [Axum host](./ch07-08-axum-host.md) — `id_effect_axum`, `execute`, JSON + schema
- [Typed routes and validation](../part6/ch29-01-typed-routes.md) — request schemas, Problem+JSON
- [Schema](../part4/ch14-00-schema.md) — wire types and `ParseError`
- Phase spec: `docs/effect-ts-parity/phases/phase-d-rpc.md`
