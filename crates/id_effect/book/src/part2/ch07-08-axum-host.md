# Axum host (`id_effect_axum`)

Workspace crate **`id_effect_axum`** runs **`Effect<A, E, R>`** programs inside **[Axum](https://docs.rs/axum)** handlers on the **same Tokio runtime** as `#[tokio::main]` / `axum::serve`.

## Mental model

- Axum stays **`async fn`** at the **wire edge**; your **domain** stays in **`Effect`** with environment **`R`** (often `caps!(…)` or `State<Env>` at the boundary).
- The bridge takes **`State<Env>`**, **builds** an effect from `&mut Env`, then drives it to completion with **`id_effect_tokio::run_async`** using **`tokio::task::block_in_place`** + **`Handle::block_on`** so the **`Effect` value never crosses a `Send` async boundary** incorrectly.

## Runtime requirements

Workspace effects are intentionally **not `Send`** in the general case. Axum handlers must return **`Send`** futures; the **`id_effect_axum`** adapter satisfies that contract by running the interpreter on the **multi-thread** runtime's blocking integration path. Use a **multi-thread** Tokio runtime (default for `#[tokio::main]`). On **`current_thread`**, prefer driving effects outside this adapter or supply a dedicated integration.

**Tests:** `#[tokio::test]` defaults to **current-thread** and can **panic** with this bridge—use e.g. `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` for router tests (see crate tests for the pattern).

## API surface

- **`routing::get` / `post` / …** — ergonomic wrappers when the handler is `Fn(&mut Env) -> Effect<…>`.
- **`run_with_caps`** — you already have `State<Env>`; build and run one effect per request.
- **`execute`** — Axum handler: `State` + `IntoResponse` for success and failure.
- **`json::decode_json_schema`** — validate JSON bodies with **`Schema::decode_unknown`** and map errors to HTTP responses (e.g. **422** with paths).

## Capability DI end-to-end

Run the reference example:

```bash
cargo run -p id_effect_axum --example 020_capability_run_with
```

Pattern:

1. `build_env([provide!(…) , …])` at startup
2. `Router::with_state(env)`
3. `run_with_caps(State(env), |env| my_effect())` per route

```rust
use axum::{Router, extract::State, routing::get};
use id_effect::{Env, build_env, caps, effect, provide, require, Effect};
use id_effect_axum::run_with_caps;

#[::id_effect::capability(u32)]
struct Counter;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(CounterKey)]
struct CounterLive;

impl CounterLive {
    fn new() -> u32 { 7 }
}

fn handler(_env: &mut Env) -> Effect<String, (), caps!(CounterKey)> {
    effect!(|r| {
        let n = ~CounterKey;
        format!("count={n}")
    })
}

let env = build_env([provide!(CounterLive)]).expect("env");
let app = Router::new()
    .route("/", get(|State(env): State<Env>| async move {
        run_with_caps(State(env), handler).await.unwrap()
    }))
    .with_state(env);
```

## Further reading

- [RPC boundaries](./ch07-12-rpc-boundaries.md) — `id_effect_rpc` envelopes, correlation ids, tracing
- `cargo doc --open -p id_effect_axum`
- Examples: `cargo run -p id_effect_axum --example 010_routing_hello`
- [Tokio bridge](./ch07-05-tokio-bridge.md) for interpreter semantics; [Tower](./ch07-09-tower-service.md) for generic `Service` composition without Axum.
