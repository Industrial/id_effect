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

## Sessions and OAuth

Trait-only v1 — bring your own store/IdP via `id_effect_platform::auth`:

- [`SessionStore`](https://docs.rs/id_effect_platform/latest/id_effect_platform/trait.SessionStore.html) + [`MemorySessionStore`](https://docs.rs/id_effect_platform/latest/id_effect_platform/struct.MemorySessionStore.html)
- [`OAuthClient`](https://docs.rs/id_effect_platform/latest/id_effect_platform/trait.OAuthClient.html) + [`MemoryOAuthClient`](https://docs.rs/id_effect_platform/latest/id_effect_platform/struct.MemoryOAuthClient.html)

## Security middleware

From `id_effect_axum::server`:

- [`csp_middleware`](https://docs.rs/id_effect_axum/latest/id_effect_axum/fn.csp_middleware.html) — Content-Security-Policy
- [`csrf_middleware`](https://docs.rs/id_effect_axum/latest/id_effect_axum/fn.csrf_middleware.html) — double-submit token on mutating verbs

## See also

- Mission: `platform-application`
- Platform auth: Part VI ch26 (`id_effect_platform::auth`)
