# `id_effect_axum`

Run [`id_effect`](../id_effect) `Effect` programs inside [Axum](https://docs.rs/axum) handlers and
[Tower](https://docs.rs/tower) services: route helpers, request-scoped capability overrides, JSON +
schema decoding, health/readiness routes and a host lifecycle (`HostBuilder`, graceful drain).

## Quick start

```rust
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use id_effect::{Effect, succeed};
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

Use `id_effect_axum::execute(State(s), |s| effect)` inside a plain Axum handler when you need extra
extractors, and `run_with_env` / `run_with_caps` when you only want the `Result`.

## Send strategy

`Effect` futures are deliberately **not** `Send`; Axum handlers must be. The crate has one answer for
every entry point (`routing::*`, `execute`, `run_with_env`, `run_with_caps`, the request-cap helpers and
the `tower` feature's `service` module): the **`EffectExecutor`**, an elastic pool of dedicated OS
threads (not Tokio's blocking pool).

- **Thread per in-flight request.** The handler sends a `Send` recipe — the state value plus the
  closure that builds the effect — to a pooled thread running its own `current_thread` Tokio runtime;
  the result comes back over a oneshot channel. Handler futures are `Send`, never block a runtime
  worker, and work on `current_thread` runtimes too.
- **Isolation.** A synchronous step that blocks (`Effect::new` doing `std::fs`, a CPU loop,
  `thread::sleep`) occupies only its own request's thread.
- **Elastic and bounded.** Threads are created on demand up to `max_threads` (default 512), reused,
  and retired after `idle_timeout` (default 10 s). Up to `queue_capacity` (default 1024) more requests
  wait for a thread. Beyond that, callers wait for admission (`SaturationPolicy::Wait`, default) or —
  with `SaturationPolicy::Reject` — routing helpers, `execute`, `execute_with_request_caps` and
  `exchange_into_response` answer **`503`** with `Retry-After: 1`. `run_with_env` / `run_with_caps` and
  Tower services always wait.
- **Run to completion.** If the client disconnects, the effect still finishes (its result is
  discarded), so nothing stops half-way between two side effects. Opt into cancel-on-drop with
  `DropPolicy::Cancel` (the effect is dropped at its next `.await`; synchronous steps cannot be
  interrupted).
- Panics inside the effect are re-raised in the handler.
- Inside an effect, `tokio::spawn` / `Handle::current()` refer to the worker thread's runtime (tasks
  live as long as that thread); resources created on the application runtime keep working.

```rust,no_run
use std::time::Duration;
use id_effect_axum::{DropPolicy, EffectExecutor, SaturationPolicy};

let executor = EffectExecutor::builder()
  .max_threads(256)
  .queue_capacity(512)
  .idle_timeout(Duration::from_secs(30))
  .saturation(SaturationPolicy::Reject) // shed load with 503
  .drop_policy(DropPolicy::RunToCompletion)
  .build();
EffectExecutor::install_global(executor).expect("install before serving");
```

## Request-scoped capabilities

`CapOverrideLayer` resolves a capability value per request (tenant, database handle, request id, …)
from the request head; `execute_with_request_caps` / `run_with_request_caps` overlay those values on the
router's base `Env` for that request's effect only:

```rust
use axum::{Router, extract::State, routing::get};
use id_effect::{Cap, Effect, Env, succeed};
use id_effect_axum::{CapOverrideLayer, RequestCaps, execute_with_request_caps};

#[derive(Clone)]
struct Tenant(String);

fn whoami(env: &mut Env) -> Effect<String, std::convert::Infallible, Env> {
  succeed(env.get::<Cap<Tenant>>().0.clone())
}

fn app(base: Env) -> Router {
  Router::new()
    .route(
      "/whoami",
      get(|st: State<Env>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
    )
    .layer(CapOverrideLayer::<Cap<Tenant>, _>::new(|parts| {
      let tenant = parts.headers.get("x-tenant")?.to_str().ok()?;
      Some(Tenant(tenant.to_owned()))
    }))
    .with_state(base)
}
```

## Features

| Feature | Description |
|---------|-------------|
| `tower` | `service::{EffectService, ChannelService}` — Tower `Service`s backed by effects (formerly `id_effect_tower`). |
| `cluster` | `cluster::ClusterControl` — Process Mesh control-plane routes (`id_effect_node`). |

## Breaking changes in 0.6

The build closure, `A` and `E` now cross to an executor thread, so `run_with_env`, `run_with_caps`
(and the routing helpers / `execute`, which already required most of this) need them to be
`Send + 'static`. Code that now fails to compile:

- closures borrowing locals — `run_with_env(st, |e| f(e, &body))` → move an owned value in:
  `run_with_env(st, move |e| f(e, &body))` (with `body` owned by the closure);
- closures capturing `Rc` / `RefCell` / other `!Send` values — create them *inside* the closure
  (the effect itself may hold them);
- success or error types containing `Rc` / `RefCell` — return `Send` types (e.g. `Arc`, `String`).

Runtime semantics that changed:

- effects no longer run on the caller's runtime: `tokio::spawn` inside an effect lands on an executor
  thread's runtime;
- the previous `block_in_place` bridge needed a multi-thread runtime; any flavour now works;
- concurrency is bounded (512 running + 1024 queued by default); saturation waits, or answers `503`
  under `SaturationPolicy::Reject`;
- effects still run to completion on client disconnect (as before) unless `DropPolicy::Cancel` is set;
- `service::EffectService::Future` / `ChannelService::Future` are now `Send`; `EffectService` gains
  `with_executor`, and its in-flight counter / permit are released on every exit path (including
  panics and cancellation).

## Examples

```bash
cargo run -p id_effect_axum --example 010_routing_hello
cargo run -p id_effect_axum --example 060_request_caps
cargo run -p id_effect_axum --features tower --example 050_tower_effect_service
```

See also crate docs on [docs.rs](https://docs.rs/id_effect_axum) and the `id_effect_sql` example
`010_axum_sql`.
