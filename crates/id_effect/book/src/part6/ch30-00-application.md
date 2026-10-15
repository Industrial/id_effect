# Application host

Axum application shell pieces live in **`id_effect_axum::server`** (lifecycle, config bootstrap, security middleware). Portable auth traits live in **`id_effect_platform::auth`**.

## Lifecycle

[`HostBuilder`](https://docs.rs/id_effect_axum/latest/id_effect_axum/struct.HostBuilder.html) loads [`HostConfig`](https://docs.rs/id_effect_axum/latest/id_effect_axum/struct.HostConfig.html) from `HOST`, `PORT`, and `SHUTDOWN_TIMEOUT_SECS`, then runs your `serve` future concurrently with SIGINT/SIGTERM:

```rust
use id_effect_axum::{HostBuilder, serve_router};

HostBuilder::new()
  .run_until_shutdown(|host| async move {
    serve_router(host, app).await
  })
  .await?;
```

See `cargo run -p id_effect_axum --example 040_app_host`.

The server's lifetime can also be a scoped `Layer`: `server_layer(app)` binds on acquire and shuts down gracefully (within `shutdown_timeout`) on release; `serve(app)`, `serve_until(app, stop)` and `serve_until_shutdown(app)` (SIGINT/SIGTERM) build on it:

```rust,ignore
use id_effect_axum::{EffectExecutor, HostConfig, HostError, provide_host_config_env, serve_until_shutdown};

let config = provide_host_config_env::<HostError>().provide_to(HostConfig::layer());
let program = serve_until_shutdown(app).provide_layer(config);
let exit = EffectExecutor::global().run_effect(program).await;
```

Each request's effect runs as one fiber on the tokio runtime serving it; a client disconnect interrupts that fiber (its finalizers run).

## Sessions and OAuth

Traits only — bring your own store/IdP via `id_effect_platform::auth` (methods return `Effect`s):

- [`SessionStore`](https://docs.rs/id_effect_platform/latest/id_effect_platform/trait.SessionStore.html) + [`MemorySessionStore`](https://docs.rs/id_effect_platform/latest/id_effect_platform/struct.MemorySessionStore.html)
- [`OAuthClient`](https://docs.rs/id_effect_platform/latest/id_effect_platform/trait.OAuthClient.html) + `MemoryOAuthClient` (in-memory test double, feature `test-util`)

## Security middleware

From `id_effect_axum::server`:

- [`csp_middleware`](https://docs.rs/id_effect_axum/latest/id_effect_axum/fn.csp_middleware.html) — Content-Security-Policy
- [`csrf_middleware`](https://docs.rs/id_effect_axum/latest/id_effect_axum/fn.csrf_middleware.html) — double-submit token on mutating verbs

## See also

- Mission: `platform-application`
- Platform auth: Part VI ch26 (`id_effect_platform::auth`)
