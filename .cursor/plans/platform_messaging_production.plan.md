---
name: Jobs SQLx Production
overview: "Heavy-mode initiative to replace `id_effect_sql` + `id_effect_sql_pg` in place on sqlx (breaking, no backward compatibility), then replace `id_effect_jobs` memory stubs with production adapters: Apalis, obix, and rdkafka—behind id_effect 3.0 capability DI."
todos:
  - id: spec-author
    content: Author .maestro/specs/platform-messaging-production.md and validate
    status: pending
  - id: wave-0-sqlx
    content: "Wave 0: leaf-adr-sqlx + leaf-sql-sqlx-replace (replace id_effect_sql* on sqlx, breaking)"
    status: pending
  - id: wave-1-scaffold
    content: "Wave 1: leaf-jobs-features + leaf-pg-pool-provider (feature flags + PgPoolKey)"
    status: pending
  - id: wave-2-adapters
    content: "Wave 2: leaf-jobs-apalis + leaf-jobs-obix-outbox (parallel)"
    status: pending
  - id: wave-3-inbox-kafka
    content: "Wave 3: leaf-jobs-obix-inbox + leaf-jobs-kafka (parallel)"
    status: pending
  - id: wave-4-journal-e2e
    content: "Wave 4: leaf-events-pg-journal + leaf-jobs-e2e-example (parallel)"
    status: pending
  - id: wave-5-docs-ci
    content: "Wave 5: leaf-jobs-book-ch31 + leaf-devenv-postgres-ci (parallel)"
    status: pending
isProject: false
---

# Platform Messaging Production + sqlx SQL Stack

## Breaking-change policy (locked)

**Replace `id_effect_sql` and `id_effect_sql_pg` in place. No backward compatibility.**

- Remove `deadpool-postgres`, `tokio-postgres`, and all deadpool-era APIs entirely
- Redesign trait surfaces (`SqlClient`, `SqlTransaction`, `SqlParam`, `SqlRow`, providers) around **sqlx** primitives as needed
- Bump crate semver and fix **all** workspace call sites in this initiative — no deprecation shims, no dual-stack feature flags
- Update ADR **in place** ([adr-sql-driver-choice.md](docs/platform/adrs/adr-sql-driver-choice.md)) — no `adr-sql-driver-choice-v2.md`
- Production messaging replaces memory stubs; no parallel v2 crates

## Executive summary

1. **sqlx SQL platform** — replace [id_effect_sql](crates/id_effect_sql/) + [id_effect_sql_pg](crates/id_effect_sql_pg/) on `sqlx::PgPool` / `sqlx::Transaction`; shared `PgPoolKey`.
2. **id_effect_jobs production** — Apalis (jobs), obix (outbox + inbox), rdkafka (Kafka); `memory` feature for unit tests only.

## Locked decisions

| Decision | Choice |
|----------|--------|
| SQL stack | sqlx 0.8 — replaces deadpool/tokio-postgres completely |
| Compatibility | **None** — break and fix all in-repo call sites |
| Outbox + inbox | obix on shared PgPool |
| Jobs | Apalis + apalis-postgres |
| Kafka | rdkafka |

## Wave 0 — Replace SQL stack

### `leaf-adr-sqlx`
Rewrite [adr-sql-driver-choice.md](docs/platform/adrs/adr-sql-driver-choice.md) in place. Decision: sqlx. No migration/compatibility section.

### `leaf-sql-sqlx-replace`
Replace **both** crates together:

| Remove | Add |
|--------|-----|
| deadpool-postgres, tokio-postgres | sqlx 0.8 (runtime-tokio, postgres, uuid) |
| Manual SqlParam/ToSql binding (if redundant) | sqlx query bindings |
| PgSqlTransaction on deadpool Object | sqlx::Transaction |

**AC:** deadpool deps gone; traits redesigned for sqlx; all workspace dependents compile; no deprecated shims; `moon run :test` green.

## Waves 1–5

Same structure as prior plan, with these deltas:

- **`leaf-pg-pool-provider`:** `PgPoolKey` only — `sqlx::PgPool`, no deadpool type
- **`leaf-jobs-apalis`:** Replace `JobRunner` trait if it misrepresents Apalis pull model — do not preserve misleading dequeue API
- **`leaf-jobs-obix-outbox`:** Delete `relay_outbox` / `MemoryOutbox` from default path — no deprecated wrappers
- **`leaf-jobs-kafka`:** `KafkaBrokerStub` memory-only for tests
- **`leaf-jobs-book-ch31`:** Full chapter rewrite (not v2)
- **`leaf-events-pg-journal`:** Same `PgPoolKey` — one pool type workspace-wide

## Out of scope

- Backward-compatible deadpool/sqlx dual stack
- `id_effect_sql_v2` or ADR v2 file
- Deprecation period for old SqlClient/SqlParam APIs
