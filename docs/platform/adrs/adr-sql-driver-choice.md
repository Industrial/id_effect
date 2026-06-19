# ADR — SQL driver choice for `id_effect_sql_pg`

**Status:** Accepted  
**Date:** 2026-06-19  
**Context:** Phase C (`@effect/sql` parity) needs one production PostgreSQL stack for `id_effect_sql_pg`. Candidates: **sqlx**, **tokio-postgres**, and **deadpool-postgres** (pool layer on tokio-postgres).

## Decision

Implement **`id_effect_sql_pg` on `tokio-postgres` + `deadpool-postgres`**, wrapped in `Effect::new_async` and `Scope`/`acquire_release` for pool lifecycle.

Do **not** adopt **sqlx** as the v1 driver inside the platform crate (users may still use sqlx directly at application edges via `from_async`).

## Options considered

### sqlx

| Aspect | Assessment |
|--------|------------|
| Connection pool | Built-in (`PgPool`); sizing and acquire timeout configurable. |
| Cancellation | Query cancellation is limited; long-running queries often run to completion unless the connection is dropped. |
| Effect integration | Opinionated high-level API; maps cleanly to `Effect` but pulls compile-time query checking and optional `DATABASE_URL` at build time. |
| Ecosystem | Migrations helper at the edge; heavy feature matrix (`runtime-tokio`, TLS backends, etc.). |

**Pros:** Ergonomic query macros, single crate for pool + queries, widely adopted.  
**Cons:** Compile-time SQL ties CI to DB availability or offline cache; larger dependency graph; harder to keep `id_effect_sql` traits driver-agnostic when the driver crate leans on macros.

### tokio-postgres

| Aspect | Assessment |
|--------|------------|
| Connection pool | None — callers must add a pool (e.g. deadpool). |
| Cancellation | Dropping the in-flight future / connection aborts work cooperatively with Tokio; behavior is predictable and documented. |
| Effect integration | Thin async API — ideal for `Effect::new_async` and explicit `SqlError` mapping without macro magic. |
| Ecosystem | Battle-tested; used by deadpool and many production services. |

**Pros:** Minimal surface, explicit control, no build-time DB coupling.  
**Cons:** Manual parameter binding and row decoding; pool is a separate concern.

### deadpool-postgres

| Aspect | Assessment |
|--------|------------|
| Connection pool | Purpose-built async pool over `tokio-postgres`; acquire timeouts, max size, metrics hooks. |
| Cancellation | Inherits tokio-postgres semantics; pool `get()` respects Tokio task cancellation. |
| Effect integration | Pool **acquire** maps naturally to `acquire_release` / `Scope` finalizers (return connection on scope close). |
| Ecosystem | Composes with tokio-postgres only — not a third wire protocol stack. |

**Pros:** Pool semantics align with `id_effect` resource patterns; lighter than sqlx for CI.  
**Cons:** Not a standalone driver — always paired with tokio-postgres.

## Comparison summary

| Criterion | sqlx | tokio-postgres | deadpool-postgres |
|-----------|------|----------------|-------------------|
| Pool (v1 requirement) | Built-in | Requires add-on | Yes |
| Cancellation | Weak / driver-dependent | Good | Good (via tokio-postgres) |
| Effect / Scope fit | Good, but bundled opinions | Excellent (thin wrap) | Excellent (acquire = resource) |
| CI / build coupling | Optional compile-time SQL | None | None |
| Trait crate separation | Macros leak into call sites | Clean boundary | Clean boundary |

## Rationale

1. **Resource safety:** `deadpool` acquire/release pairs map directly onto `id_effect::acquire_release` and `Scope` finalizers planned for transaction leaves (`leaf-sql-transaction-scope`).
2. **Cancellation:** Platform spec requires documenting cancellation limits; tokio-postgres drop semantics are easier to reason about in `Effect` fibers than sqlx's partial cancellation story.
3. **Effect integration:** Driver-agnostic traits live in `id_effect_sql`; the PG crate should be a thin `Effect` facade over async I/O — tokio-postgres keeps that boundary sharp.
4. **Testing:** No compile-time DB URL requirement keeps default workspace CI green; integration tests can stay feature-gated behind Docker/testcontainers.

## Consequences

- **`id_effect_sql_pg`** depends on `tokio-postgres`, `deadpool-postgres`, and a TLS backend (e.g. `rustls` via `tokio-postgres` features) — chosen in the driver leaf.
- Row decoding stays manual (or small helpers in the driver crate); no sqlx `FromRow` derive in v1.
- Application teams wanting sqlx macros may continue using sqlx outside `id_effect_sql` until/unless a community adapter appears.
- Future ADR required before adding a second driver or switching to sqlx as the official PG implementation.

## References

- `.maestro/specs/platform-data.md`
- `docs/effect-ts-parity/phases/phase-c-sql.md`
- Effect.ts `@effect/sql` / `@effect/sql-pg`
