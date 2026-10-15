# `id_effect_axum`

Run [`id_effect`](../id_effect) `Effect` programs (the 0.7 kernel, `id_effect::next`) inside
[Axum](https://docs.rs/axum) handlers and [Tower](https://docs.rs/tower) services: route helpers,
request-scoped capability overrides, streaming bodies, JSON + schema decoding, health/readiness
routes, and the HTTP server as a scoped `Layer`.

## Quick start

```rust
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use id_effect::next::{Effect, succeed};
use id_effect_axum::routing;
use tower::ServiceExt;

#[derive(Clone)]
struct AppState {
  greeting: &'static str,
}

fn hello(env: &mut AppState) -> Effect<String, std::convert::Infallible, AppState> {
  succeed(format!("{}, world", env.greeting))
}

// `#[tokio::main]`, `#[tokio::test]` (current-thread) — any runtime flavour works.
#[tokio::main(flavor = "current_thread")]
async fn main() {
  let app = Router::new()
    .route("/hello", routing::get(hello))
    .with_state(AppState { greeting: "hello" });

  let res = app
    .oneshot(Request::builder().uri("/hello").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(res.status(), StatusCode::OK);
}
```

The router state is the effect's environment: plain state, a typed capability list
(`HCons<Db, HNil>`, read with `need::<Db>()`) or a runtime-checked `DynEnv`. Use
`id_effect_axum::execute(State(s), |s| effect)` inside a plain Axum handler when you need extra
extractors, and `run_with_env` / `run_with_caps` when you want the `Exit` instead of a response.

## Execution

`next` effects are `Send`, so each request's effect runs as **one fiber on the tokio runtime that
serves the request** — no thread pool, no `LocalSet`, no `block_in_place`.

- **Outcomes.** Success → `A::into_response()`; a typed failure → `E::into_response()`; a defect (a
  panic anywhere in the handler or its effect) → `500 Internal Server Error` with a `HandlerDefect`
  response extension for logging middleware (the message is never sent to the client); an
  interrupt → `503`.
- **Interruption.** When the request future is dropped (client disconnect, timeout layer,
  shutdown), the fiber is interrupted and its finalizers (`ensuring`, `acquire_release`, scoped
  layers) run (`DropPolicy::Interrupt`, default). `DropPolicy::Detach` lets it finish instead.
- **Backpressure** is opt-in: `max_in_flight(n)` caps running request fibers; callers wait, or with
  `SaturationPolicy::Reject` the routing helpers, `execute`, `execute_with_request_caps` and
  `exchange_into_response` answer **`503`** with `Retry-After: 1`.

```rust,no_run
use id_effect_axum::{DropPolicy, EffectExecutor, SaturationPolicy};

let executor = EffectExecutor::builder()
  .max_in_flight(1024)
  .saturation(SaturationPolicy::Reject) // shed load with 503
  .drop_policy(DropPolicy::Interrupt)
  .build();
EffectExecutor::install_global(executor).expect("install before serving");
```

## Request-scoped capabilities

`CapOverrideLayer` resolves a capability value per request (tenant, database handle, request id, …)
from the request head; `execute_with_request_caps` / `run_with_request_caps` overlay those values on
the router's base `DynEnv` for that request's effect only:

```rust
use axum::{Router, extract::State, routing::get};
use id_effect::next::{DynEnv, Effect, succeed};
use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps};

struct Tenant(String);

fn whoami(env: &mut DynEnv) -> Effect<String, std::convert::Infallible, DynEnv> {
  succeed(env.try_get::<Tenant>().map_or("none".into(), |t| t.0.clone()))
}

fn app() -> Router {
  Router::new()
    .route(
      "/whoami",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
    )
    .layer(CapOverrideLayer::new(|parts| {
      let tenant = parts.headers.get("x-tenant")?.to_str().ok()?;
      Some(Tenant(tenant.to_owned()))
    }))
    .with_state(DynEnv::new().with(Tenant("public".into())))
}
```

## Streaming bodies

`body::stream_body(stream)` turns a `next::Stream<Bytes, E>` into a response `Body` (a producer
fiber feeds it chunk by chunk; dropping the body interrupts the producer); `body::body_stream(body)`
reads a request body as a stream.

## The server as a layer

`HostConfig::layer()` loads `HOST` / `PORT` / `SHUTDOWN_TIMEOUT_SECS` through `id_effect_config`;
`server_layer(app)` binds on acquire and shuts down gracefully on release; `serve(app)` launches it
until interrupted and `serve_until_shutdown(app)` until SIGINT/SIGTERM:

```rust,no_run
use id_effect::next::Effect;
use id_effect_axum::{
  EffectExecutor, HostConfig, HostError, ShutdownReason, health_router, provide_host_config_env,
  serve_until_shutdown,
};

# async fn run() {
let config = provide_host_config_env::<HostError>().provide_to(HostConfig::layer());
let program: Effect<ShutdownReason, HostError> =
  serve_until_shutdown(health_router()).provide_layer(config);
let exit = EffectExecutor::global().run_effect(program).await;
# let _ = exit;
# }
```

## Features

| Feature | Description |
|---------|-------------|
| `tower` | `service::{EffectService, ChannelService}` — Tower `Service`s backed by effects. |
| `cluster` | `cluster::ClusterControl` — Process Mesh control-plane routes (`id_effect_node`). |

## Migrating from 0.6

See [`docs/migration-0.7/m-axum.md`](../../docs/migration-0.7/m-axum.md).

## Examples

```bash
cargo run -p id_effect_axum --example 010_routing_hello
cargo run -p id_effect_axum --example 060_request_caps
cargo run -p id_effect_axum --features tower --example 050_tower_effect_service
```
