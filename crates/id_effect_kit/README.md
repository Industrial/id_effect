# `id_effect_kit`

One front door to [`id_effect`](../id_effect): a single dependency with a feature flag per battery, whole-crate re-exports and a `prelude`.

```toml
[dependencies]
id_effect_kit = { version = "0.9", features = ["web", "otel"] }
# The derive and `effect!` macros expand to `::id_effect::` paths; keep the core crate too.
id_effect = "0.9"
```

| Feature | Adds | Re-exported as |
|---|---|---|
| always on | `id_effect`, `id_effect_cli` (`run_main`), `id_effect_config` (`Secret`) | `id_effect_kit::{id_effect, cli, config}` |
| `web` | `id_effect_axum` (typed routes, Problem+JSON, middleware) | `id_effect_kit::axum` |
| `openapi` | `web` + `/openapi.json` and `/docs` from the typed routes | `id_effect_kit::axum::openapi` |
| `sql` | `id_effect_sql` without a driver (`TestSqlClient`, transactions, `FromRow`) | `id_effect_kit::sql` |
| `sql-pg`, `sql-sqlite`, `sql-mysql` | `sql` + the `pg` / `sqlite` / `mysql` driver | `id_effect_kit::sql::{pg, sqlite, mysql}` |
| `migrate` | `sql` + versioned migrations (`migrate::run`, `migrate::layer`) | `id_effect_kit::sql::migrate` |
| `repository` | `sql` + `#[derive(Entity)]`, `Repository<T>` | `id_effect_kit::sql::repository` |
| `audit` | `sql` + the hash-chained audit log appended in the caller's transaction (PostgreSQL) | `id_effect_kit::sql::audit` |
| `otel` | `id_effect_opentelemetry` (OTLP, config loading) | `id_effect_kit::otel` |
| `cluster` | `id_effect_node` (with `web`: also `axum::cluster`) | `id_effect_kit::node` |
| `auth` | `web` + `id_effect_auth` with Axum middleware and JWTs (with `sql-pg`: `PgSessionStore`; with `redis`: the Redis session store) | `id_effect_kit::auth` |
| `auth-oidc`, `auth-totp` | `auth` + OpenID Connect and JWKS / TOTP | `id_effect_kit::auth::{oidc, totp}` |
| `authz` | `id_effect_authz` (with `web`: route guards) | `id_effect_kit::authz` |
| `redis` | `id_effect_redis` (with `web`: the rate-limit store) | `id_effect_kit::redis` |
| `config-vault` | the Vault provider of `id_effect_config` | `id_effect_kit::config::vault` |
| `test` | `id_effect_test` (containers, isolated databases, fixtures, HTTP client) — for `[dev-dependencies]` | `id_effect_kit::test` |
| `full` | all of the above except `test`; builds without Docker or system libraries | |

Enable `test` on a `[dev-dependencies]` entry of the kit (`id_effect_kit = { version = "0.9", features = ["test"] }`), so containers and fixtures stay out of the shipped binary.

## Quick start

```rust
use std::process::ExitCode;

use id_effect_kit::prelude::*;

#[derive(Debug)]
enum AppError {
  Unconfigured,
}

fn token() -> Effect<Secret<String>, AppError> {
  succeed(Secret::new("s3cr3t".to_string()))
}

fn program() -> Effect<usize, AppError> {
  token().flat_map(|t| {
    if t.expose().is_empty() {
      fail(AppError::Unconfigured)
    } else {
      succeed(t.expose().len())
    }
  })
}

fn main() -> ExitCode {
  // Secrets never print: `Debug` shows `<redacted>`.
  assert_eq!(format!("{:?}", Secret::new("x")), "<redacted>");
  run_main(program(), hlist![], RunMainConfig::minimal())
}
```

The per-feature prelude items: `web` adds `EffectExecutor`, `execute`, `routing`, `serve_until_shutdown`, `SecurityLayer`, `health_router`, `readiness_router`, `observability_routes`, `metrics_router`, the typed routes (`Route`, `Router`, `Input`, `Problem`, `ValidPath`, `ValidQuery`, `ValidHeaders`, `ValidJson`, `ValidForm`) and the middleware (`RequestIdLayer`, `TimeoutLayer`, `BodyLimitLayer`, `CompressionLayer`); `openapi` adds `OpenApi` and `openapi_router`; `sql` adds `SqlClientService`, `SqlError`, `SqlRow`, `FromRow`, `params!` and `with_transaction`; `migrate` adds `Migrator` and `MigrateError`; `repository` adds `Entity` and `Repository`; `otel` adds `OtelConfig` and `install_from_config`; `auth` adds `Authenticate`, `AuthPrincipal`, `AuthError`, `Sessions`, `SessionLayer`, `PasswordHasher`, `JwtIssuer`, `JwtVerifier` and `JwtKey`; `authz` adds `Policies`, `Policy`, `Principal`, `AuthzError`, `CurrentPrincipal`, `authorize`, `require_role` (and `AuthorizeExt` with `web`); `redis` adds `RedisClient` and `RedisConfig`. `Profiles` (config profiles) is always there. Through the kit alone, point the `FromRow` / `Entity` derives at the re-export with `#[from_row(crate = id_effect_kit::sql)]` / `#[entity(crate = id_effect_kit::sql, …)]`.

## Tier

`integration` (ADR 0010 Amendment 4). It moves to `platform` (ADR 0012) once a reference app under `examples/reference/` depends on it.

## Examples

`cargo run -p id_effect_kit --example 010_prelude` (add `--features full` for the battery section).
