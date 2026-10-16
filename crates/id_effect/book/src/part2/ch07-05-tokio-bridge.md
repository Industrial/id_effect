# Tokio bridge (`id_effect_tokio`)

The core `id_effect` crate already runs effects on Tokio: `Effect::block_on` and
`Runtime::global()` use a `TokioExecutor`, and every fiber is an ordinary Tokio task with a real
waker. Workspace crate **`id_effect_tokio`** adds **`TokioRuntime`**, a handle that ties an
`id_effect::Runtime` to a specific Tokio runtime — the one your `#[tokio::main]` is running, or one
it owns.

Read this section **before** [Platform I/O](./ch07-06-platform-services.md),
[HTTP via reqwest](./ch07-07-reqwest-http.md) and [Axum](./ch07-08-axum-host.md).

## What `TokioRuntime` provides

| Method | What it does |
|--------|--------------|
| `TokioRuntime::current()` | wrap the ambient Tokio runtime (inside `#[tokio::main]` / `#[tokio::test]`) |
| `new_current_thread()`, `new_multi_thread()`, `from_handle(h)` | own a runtime, or wrap a handle |
| `rt.run(eff).await` | run an `Effect<A, E, HNil>` as a root fiber → `Exit<A, E>` |
| `rt.fork(eff)` | start a root fiber → `FiberHandle` (`join_future().await`, `interrupt_now()`) |
| `rt.spawn(eff)` | → `tokio::task::JoinHandle<Exit<A, E>>` |
| `rt.block_on(fut)` | drive a future on an **owned** runtime |
| `rt.runtime()`, `rt.executor()` | the underlying `id_effect::Runtime` / `TokioExecutor` |

Effects are `Send`, so `rt.run(eff)` is a `Send + 'static` future you can await anywhere — no
`spawn_blocking` adapters. `sleep` and `yield_fiber` use the fiber's executor, so on a
`TokioRuntime` they park on Tokio timers.

## Capabilities with Tokio

Provide `R` first (`provide`, `provide_all`, `provide_layer`), then run the closed effect
(`crates/id_effect_tokio/examples/109_tokio_end_to_end.rs`):

```rust
use id_effect::{Effect, Exit, HCons, HNil, from_async, hlist, need};
use id_effect_tokio::TokioRuntime;
# struct Quote;
# enum AppError { MissingApiToken }
# async fn load_quotes() -> Vec<Quote> { vec![Quote] }

struct ApiToken(&'static str);
type AppEnv = HCons<ApiToken, HNil>;

fn fetch_quotes() -> Effect<Vec<Quote>, AppError, AppEnv> {
    need::<ApiToken>().flat_map(|token| {
        let missing = token.0.is_empty();
        from_async(move || async move {
            if missing {
                return Err(AppError::MissingApiToken);
            }
            Ok(load_quotes().await)
        })
    })
}

// Inside `#[tokio::main] async fn main()`, or any task on a Tokio runtime:
async fn serve() {
    let rt = TokioRuntime::current().expect("inside a tokio runtime");
    let exit = rt.run(fetch_quotes().provide_all(hlist![ApiToken("secret")])).await;
    assert!(matches!(exit, Exit::Success(_)));
}
# let owned = TokioRuntime::new_current_thread().expect("tokio runtime");
# owned.block_on(serve());
```

For layer-based apps: `rt.run(app().provide_layer(live_layer())).await`.

## Mental model

| Concern | Where it lives |
|---------|----------------|
| Describing work | `Effect<A, E, R>` |
| Capabilities | `caps!(…)` / `HCons` in `R`, built with `Layer`s or provided as values |
| Blocking edge (`main`, sync tests) | `eff.block_on()` |
| Async edge on Tokio | `TokioRuntime::current()?.run(eff).await` or `Runtime::global().run(eff).await` |
| Deterministic tests | `TestExecutor` (virtual clock), see [Testing](../part4/ch15-00-testing.md) |

## Sharp edges

1. **`block_on` inside async code is refused.** `Effect::block_on` parks the calling thread; inside
   a Tokio task it fails with a `BlockOnInAsync` defect (`try_block_on` returns the error). Await
   `rt.run(eff)` there instead.
2. **IO drivers.** Live platform effects (reqwest, `tokio::process`) need a Tokio runtime with its
   IO driver; `TokioRuntime`'s owned runtimes enable every compiled driver.

## Further reading

- Examples: `cargo run -p id_effect_tokio --example 109_tokio_end_to_end` (also `106`–`108`)
- `cargo doc --open -p id_effect_tokio`
