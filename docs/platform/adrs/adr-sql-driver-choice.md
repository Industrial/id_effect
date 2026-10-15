# ADR — SQL driver choice for `id_effect_sql_pg`

**Status:** Accepted  
**Date:** 2026-06-19 (revised)  
**Context:** Phase C (`@effect/sql` parity) and platform messaging production require one PostgreSQL stack shared by SQL traits, Apalis jobs, obix outbox/inbox, and the event journal.

> **Current state (0.7):** the decision (sqlx 0.8) still holds, but the names below are historical.
> `id_effect_sql_pg` was merged into `id_effect_sql` behind the `pg` feature in 0.6
> (`id_effect_sql::pg`), and `Effect::new_async` is gone: since 0.7 the sqlx futures are lifted with
> `id_effect::from_async`, and the client is built by the scoped `PgSqlClient::layer(config)`.
> The jobs and events crates (`id_effect_jobs`, `id_effect_events`) moved to the unpublished
> `labs/` workspace in 0.6 ([ADR 0010](../../adrs/0010-crate-tiers.md)).

## Decision

Implement **`id_effect_sql_pg` on `sqlx` 0.8** (`PgPool`, `PoolConnection`, runtime-tokio) wrapped in `Effect::new_async` (0.7: `from_async`).

Remove **deadpool-postgres** and **tokio-postgres** from the platform crate entirely. No backward-compatibility shims.

## Rationale

1. **One pool type workspace-wide** — `sqlx::PgPool` via [`PgPool`](../../../crates/id_effect_sql/src/pg/pool_key.rs) feeds Apalis, obix, and `EsEntityPgBackend` without adapter layers.
2. **Ecosystem alignment** — Apalis-postgres, obix, and sqlx migrations share the same driver stack.
3. **Effect integration** — `SqlClient` / `SqlTransaction` remain thin `Effect` facades over sqlx query/transaction APIs.

## Consequences

- **`id_effect_sql_pg`** depends on `sqlx` with `runtime-tokio`, `postgres`, and `uuid`.
- Row decoding stays manual in `PgSqlClient` (no compile-time query macros in the platform crate).
- Semver bump on `id_effect_sql_pg`; all in-repo call sites updated in the messaging production initiative.

## References

- `.cursor/plans/platform_messaging_production.plan.md`
- `.maestro/specs/platform-data.md`
- `docs/effect-ts-parity/phases/phase-c-sql.md`
