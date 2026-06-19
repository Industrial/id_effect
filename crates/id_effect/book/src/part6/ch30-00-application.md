# Application host

[`id_effect_host`](../../../id_effect_host) wraps lifecycle, config bootstrap, auth trait surfaces, and security middleware for Axum services.

## Lifecycle

[`HostBuilder`](https://docs.rs/id_effect_host/latest/id_effect_host/struct.HostBuilder.html) loads [`HostConfig`](https://docs.rs/id_effect_host/latest/id_effect_host/struct.HostConfig.html) from `HOST`, `PORT`, and `SHUTDOWN_TIMEOUT_SECS`, then runs your `serve` future until SIGINT/SIGTERM:

```rust
use id_effect_host::{HostBuilder, HostError};

HostBuilder::new()
  .run_until_shutdown(|host| async move {
    // bind axum::serve with host.config.bind_port
    Ok(())
  })
  .await?;
```

## Sessions and OAuth

Trait-only v1 — bring your own store/IdP:

- [`SessionStore`](https://docs.rs/id_effect_host/latest/id_effect_host/trait.SessionStore.html) + [`MemorySessionStore`](https://docs.rs/id_effect_host/latest/id_effect_host/struct.MemorySessionStore.html)
- [`OAuthClient`](https://docs.rs/id_effect_host/latest/id_effect_host/trait.OAuthClient.html) + [`MemoryOAuthClient`](https://docs.rs/id_effect_host/latest/id_effect_host/struct.MemoryOAuthClient.html)

## Security middleware

- [`csp_middleware`](https://docs.rs/id_effect_host/latest/id_effect_host/fn.csp_middleware.html) — Content-Security-Policy
- [`csrf_middleware`](https://docs.rs/id_effect_host/latest/id_effect_host/fn.csrf_middleware.html) — double-submit token on mutating verbs

## See also

- Mission: `platform-application`
