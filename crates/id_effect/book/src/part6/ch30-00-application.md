# Application host

Axum application shell pieces live in **`id_effect_axum::server`** (lifecycle, config bootstrap, security middleware). Portable auth traits live in **`id_effect_platform::auth`**.

## Lifecycle

[`HostBuilder`](https://docs.rs/id_effect_axum/latest/id_effect_axum/struct.HostBuilder.html) loads [`HostConfig`](https://docs.rs/id_effect_axum/latest/id_effect_axum/struct.HostConfig.html) from `HOST`, `PORT`, and `SHUTDOWN_TIMEOUT_SECS`, then runs your `serve` future concurrently with SIGINT/SIGTERM:

```rust,ignore
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

`id_effect_axum::server::security` is secure by default. One layer turns everything on:

```rust,ignore
use id_effect_axum::server::security::{CookieKey, CorsConfig, SecurityLayer};

let app = Router::new()
  .route("/notes", get(list).post(create))
  .route("/webhooks/stripe", post(stripe))
  .layer(SecurityLayer::recommended(key).csrf_exempt("/webhooks"))
  .layer(CorsConfig::new().allow_origin("https://app.example").allow_credentials(true).build()?);
```

[`SecurityLayer::recommended(key)`](https://docs.rs/id_effect_axum/latest/id_effect_axum/server/security/struct.SecurityLayer.html) bundles:

- **CSRF** — each client session gets a random 32-byte token in a signed `__Host-csrf` cookie (`Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/`). Every request that is not `GET`/`HEAD`/`OPTIONS`/`TRACE` must echo it in `X-CSRF-Token` or a `csrf_token` form field. The two are compared in constant time. A cookie the server did not sign never matches. This is a synchronizer token kept in a signed cookie, not a double-submit check. Handlers read the token with the `CsrfToken` extractor. Opt out per path prefix with `csrf_exempt(prefix)` or entirely with `without_csrf()`.
- **CSP** — `ContentSecurityPolicy::recommended()` (`default-src 'self'`, scripts from `'self'` or with this request's nonce, `object-src 'none'`, `frame-ancestors 'none'`, ...). The nonce is fresh per request and available through the `CspNonce` extractor. `script_hash(content)` allows one exact inline script. Replace the policy with `.csp(policy)?` (validated, typed error) or drop it with `without_csp()`.
- **Security headers** — HSTS, `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`. Each is set only when the handler did not set it, so a handler can override any of them.
- **Cookies** — the `Cookies` extractor applies `CookieDefaults` (`HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/`) to cookies a handler adds, and offers signed and private (encrypted) jars.

The `CookieKey` that signs and encrypts cookies is a capability. `CookieKey::layer()` builds it from config (`COOKIE_KEY`: base64 of at least 64 random bytes; a short or malformed key is a `ConfigError`). `CookieKey::layer_from(key)` provides a key you already hold.

`CorsConfig` builds a `tower_http` CORS layer. Allowing credentials with a wildcard origin is a build error, not a runtime panic.

The 0.7 `CsrfConfig`, `csrf_middleware`, `csp_middleware` and `set_csrf_header` still compile in 0.8. They are deprecated and will be removed in 0.9. The static token compare now runs in constant time.

## See also

- Mission: `platform-application`
- Platform auth: Part VI ch26 (`id_effect_platform::auth`)
