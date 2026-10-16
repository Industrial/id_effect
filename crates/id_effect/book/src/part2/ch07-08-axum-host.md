# Axum host (`id_effect_axum`)

Workspace crate **`id_effect_axum`** runs **`Effect<A, E, R>`** programs inside
**[Axum](https://docs.rs/axum)** handlers on the **same Tokio runtime** that serves the request.

## Mental model

- Axum stays **`async fn`** at the **wire edge**; your **domain** stays in **`Effect`**.
- The router **state** `S` is the handler's environment: a handler is
  `Fn(&mut S) -> Effect<A, E, S>`. `S` can be plain state, a typed capability list
  (`HCons<Db, HNil>`, read with `need::<Db>()`), or a runtime-checked `DynEnv`.
- Effects are `Send`, so each request's effect runs as **one fiber** on the serving runtime (an
  `EffectExecutor` over the ambient Tokio handle). Any runtime flavour works; there is no
  `block_in_place` bridge.

## Outcomes

| Effect outcome | HTTP response |
|----------------|---------------|
| success `A` | `A::into_response()` |
| typed failure `E` | `E::into_response()` |
| defect (a panic in the build closure or the effect) | `500`, with a `HandlerDefect` response extension (the message is never sent) |
| interrupt | `503` |

When the client disconnects, the request fiber is **interrupted** (`DropPolicy::Interrupt`, the
default), so its finalizers (`ensuring`, `acquire_release`, scoped layers) run.
`DropPolicy::Detach` lets it run to completion. `EffectExecutor::builder()` also sets
`max_in_flight(n)` with `SaturationPolicy::{Wait, Reject}` (503).

## API surface

- **`routing::get` / `post` / `put` / `patch` / `delete` / `on`** — method routers for
  `Fn(&mut S) -> Effect<A, E, S>` handlers; `*_with_metrics(Counter, Histogram, f)` variants.
- **`execute(State(s), build)`** — inside your own `async` handler: run one effect and turn its
  `Exit` into a `Response`.
- **`run_with_env(s, build)` / `run_with_caps(State<DynEnv>, build)`** — run one effect and get the
  `Exit<A, E>` back (`into_result()` for `Result<A, Cause<E>>`).
- **`RequestCaps` + `CapOverrideLayer`** — per-request capabilities over a `DynEnv`
  (`execute_with_request_caps`).
- **`route`** — typed routes: `Route::get(..).path::<P>().query::<Q>().body::<B>().handler(f)`
  decodes every request part through a `Schema` before `f` runs, and answers invalid input with an
  RFC 9457 `Problem` (**422**, `application/problem+json`) listing every issue with a JSON Pointer.
  `route::Router` keeps a registry of the routes; `ValidPath` / `ValidQuery` / `ValidHeaders` /
  `ValidJson` / `ValidForm` do the same decoding in plain axum handlers. See
  [Typed routes and validation](../part6/ch29-01-typed-routes.md).
- **`json::decode_json`** — decode a JSON body with a `Schema`, failing with the same `Problem`
  (it replaces the deprecated `decode_json_schema`).
- **`body::{stream_body, body_stream}`** — response bodies from a `Stream<Bytes, E>` (dropping the
  body interrupts the producer), request bodies as a stream.
- **`server`** — the server as a scoped layer: `server_layer(app)`, `serve(app)`,
  `serve_until_shutdown(app)` (SIGINT/SIGTERM, graceful drain), `HostConfig` from
  [`id_effect_config`](./ch07-10-config.md).

## Typed capabilities as router state

From `examples/020_capability_run_with.rs`:

```rust
use axum::Router;
use id_effect::{Effect, HCons, HNil, hlist, need};
use id_effect_axum::routing;
use std::convert::Infallible;

#[derive(Clone, Copy, Debug)]
struct Counter(u32);
type AppEnv = HCons<Counter, HNil>;

fn handler(_env: &mut AppEnv) -> Effect<String, Infallible, AppEnv> {
    need::<Counter>().map(|c| format!("count={}", c.0))
}

let app = Router::new()
    .route("/", routing::get(handler))
    .with_state(hlist![Counter(7)]);
# let _: Router = app;
```

## Serving with a layer

The server's lifetime is a scoped layer, so it composes with the rest of the application's
layers and shuts down gracefully when the program ends:

```rust
use id_effect_axum::{EffectExecutor, HostConfig, HostError, provide_host_config_env, serve_until_shutdown};
# async fn run(app: axum::Router) {

let config = provide_host_config_env::<HostError>().provide_to(HostConfig::layer());
let program = serve_until_shutdown(app).provide_layer(config);
let exit = EffectExecutor::global().run_effect(program).await;
# }
```

`examples/http-showcase` is a complete service built this way (config, Postgres, metrics,
OpenTelemetry, graceful shutdown).

## Further reading

- Examples: `cargo run -p id_effect_axum --example 010_routing_hello` (also `020`–`071`), and
  `080_typed_routes` for typed routes
- [Typed routes and validation](../part6/ch29-01-typed-routes.md) — schemas per request part,
  Problem+JSON errors, the route registry
- [RPC boundaries](./ch07-12-rpc-boundaries.md) — `id_effect_rpc` envelopes, correlation ids, tracing
- [Tower service](./ch07-09-tower-service.md) for generic `Service` composition without Axum
- `cargo doc --open -p id_effect_axum`
