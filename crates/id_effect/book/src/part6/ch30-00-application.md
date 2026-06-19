# Hosting a production HTTP service

You have domain logic in `Effect<A, E, R>` and an Axum router from [Axum host](../part2/ch07-08-axum-host.md). The next gap is **process lifecycle**: reading bind address and shutdown timeouts from the environment, stopping cleanly on SIGTERM, and layering session and security middleware without scattering that code across `main`.

The `id_effect_host` crate wraps those concerns so your binary stays a thin composition root.

## What you'll learn

- When to use `HostBuilder` instead of hand-rolled `axum::serve` + signal handling.
- How host configuration maps to environment variables.
- How to plug in session and OAuth traits (or the in-memory stubs for tests).
- Which security middleware ships with the crate and where it belongs in the stack.

## Prerequisites

- [A Complete DI Example](../part2/ch07-04-complete-example.md) — wiring `Env` at the edge.
- [Axum host](../part2/ch07-08-axum-host.md) — running effects inside handlers.

## When you need a host crate

| Situation | Approach |
|-----------|----------|
| Local prototype, single route | `#[tokio::main]` + `axum::serve` is enough |
| Service with env-driven port, graceful shutdown, shared middleware | `id_effect_host::HostBuilder` |
| You already own all of the above in a company framework | Keep your framework; borrow ideas from host config only |

The host crate does **not** replace Axum routing or effect interpreters—it standardizes **how the process starts and stops**.

## Configuration and lifecycle

`HostBuilder` loads `HostConfig` from `HOST`, `PORT`, and `SHUTDOWN_TIMEOUT_SECS`, then runs your `serve` future until SIGINT or SIGTERM:

```rust,no_run
use id_effect_host::HostBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    HostBuilder::new()
        .run_until_shutdown(|host| async move {
            // bind with host.config.bind_addr() and axum::serve(...)
            Ok(())
        })
        .await?;
    Ok(())
}
```

Set `PORT=0` in tests if you need an ephemeral port; read `host.config` inside the closure to build your router.

## Sessions and authentication (trait boundaries)

Version 1 is intentionally **bring-your-own-store**:

- `SessionStore` — persist session IDs and payloads (production: Redis, SQL; tests: `MemorySessionStore`).
- `OAuthClient` — authorization URL and token exchange (production: your IdP client; tests: `MemoryOAuthClient`).

Domain handlers stay in `Effect`; the host only exposes hooks so middleware can read the same session type you configure at startup. Implement the traits on your infrastructure types and pass them into whatever builder API your binary uses alongside `HostBuilder`.

## Security middleware

Two helpers ship for common HTTP hardening:

- **`csp_middleware`** — sets a Content-Security-Policy header appropriate for API or HTML responses you control.
- **`csrf_middleware`** — double-submit cookie pattern on mutating verbs (`POST`, `PUT`, `PATCH`, `DELETE`).

Apply them **outside** effect-driven handlers (Tower/Axum layer order), after routing and before business logic. They do not inspect `Effect` failures; map those inside handlers via [RPC boundaries](../part2/ch07-12-rpc-boundaries.md) or your error type.

## Putting it together

A typical production binary:

1. Build `Env` with providers ([Part II](../part2/ch07-03-providing-services.md)).
2. Create the Axum router with `id_effect_axum` helpers.
3. Wrap `axum::serve` in `HostBuilder::run_until_shutdown`.
4. Install CSP/CSRF layers on the router before serve.

Run integration tests with `MemorySessionStore` and a short `SHUTDOWN_TIMEOUT_SECS` so shutdown paths are covered without a real IdP.

## Summary

`id_effect_host` answers **how the OS process behaves**, not how domain effects compose. Use it when configuration, signals, and cross-cutting HTTP security should live in one place; keep business rules in `Effect` and wire them through Axum as in earlier chapters.

## Next steps

- [Async messaging and jobs](./ch31-00-async-messaging.md) — background work and outbox patterns after your HTTP surface is up.
