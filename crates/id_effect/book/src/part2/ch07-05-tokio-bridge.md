# Tokio bridge (`id_effect_tokio`)

The core **`id_effect`** crate defines interpreters (`run_blocking`, `run_async`, `run_fork`, …) and the **`Runtime`** trait. Workspace crate **`id_effect_tokio`** supplies the **Tokio-backed** implementation for binaries on **`#[tokio::main]`**.

Read this section **before** [Platform I/O](./ch07-06-platform-services.md), [HTTP via reqwest](./ch07-07-reqwest-http.md), and [Axum](./ch07-08-axum-host.md).

## What `id_effect_tokio` provides

- **`TokioRuntime`** — implements [`id_effect::Runtime`]: cooperative sleep/yield; forked fibers on Tokio's blocking pool.
- **Re-exports** — `run_async`, `run_blocking`, `run_fork`, `yield_now` for use at async boundaries.
- **`spawn_blocking_run_async`** — when the effect graph is not `Send` but must be driven by `run_async` (Axum uses the same pattern).

## Capability DI with Tokio

Build [`Env`](../../src/capability/env.rs) manually or via [`run_with`](../../src/capability/run.rs) / [`build_env`](../../src/capability/run.rs), then pass it to `run_async`:

```rust
use id_effect::{Env, caps, effect, require, run_async, succeed};

#[::id_effect::capability(&'static str)]
struct ApiToken;

fn fetch() -> Effect<Vec<Quote>, AppError, caps!(ApiTokenKey)> {
    effect!(|r| {
        let token = ~ApiTokenKey;
        // async steps…
        Ok(quotes)
    })
}

#[tokio::main]
async fn main() {
    let mut env = Env::new();
    env.insert::<ApiTokenKey>("secret");
    let quotes = run_async(fetch(), env).await?;
}
```

For provider-based apps, use `build_env` + `run_async`:

```rust
let env = build_env([provide!(ConfigLive), provide!(HttpClientLive)])?;
let res = run_async(my_handler(), env).await?;
```

## Mental model

| Concern | Where it lives |
|---------|----------------|
| Describing work | `Effect<A, E, R>` |
| Capabilities | `caps!(…)` + `Needs<K>` + `run_with` / `build_env` |
| Blocking / tests | `run_blocking(effect, env)` |
| Async I/O on Tokio | `id_effect::run_async(effect, env)` |

## Sharp edges

1. **`Send`**: `run_async` futures are often **not** `Send` — use Axum/Tower adapters or `spawn_blocking_run_async`.
2. **Router tests**: prefer `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` when exercising Axum + effects.

## Further reading

- Example: `crates/id_effect_tokio/examples/109_tokio_end_to_end.rs`
- `cargo doc --open -p id_effect_tokio`
