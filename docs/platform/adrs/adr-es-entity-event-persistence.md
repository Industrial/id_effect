# ADR — es-entity as production event persistence

**Status:** Accepted  
**Date:** 2026-06-17  
**Context:** The fp-events initiative replaces the bespoke `PgSqlJournalBackend` / `event_journal` DDL path with a library-backed persistence layer on the shared [`PgPoolKey`](../../../crates/id_effect_sql_pg/src/pool_key.rs). obix already depends on **es-entity** 0.10.x via the messaging stack.

## Decision

Use **[es-entity](https://github.com/GaloyMoney/es-entity)** on `sqlx` 0.8 as the production event persistence engine for `id_effect_events`:

1. **`EsEntityPgBackend`** — transactional append/read via `es_entity::DbOp` on the shared pool.
2. **`SqlEventJournal` / `EventStore`** — thin Effect facades; no reimplementation of CQRS or aggregate modeling inside `id_effect`.
3. **`provide_es_entity_events`** — registers journal backend + optional typed store in the capability environment.

Do **not** adopt `esrs`, `eventastic`, `fmodel-rust`, or `autumn-harvest` for this initiative.

## Rationale

1. **Shared pool model** — Same `PgPoolKey` as Apalis, obix, and duroxide-pg migrations.
2. **Transactional `DbOp`** — Command handlers can persist events in one commit boundary (optional obix outbox in the same transaction).
3. **Ecosystem alignment** — obix inbox/outbox already uses es-entity idempotency types.

## Non-goals

- Replacing obix outbox with es-entity aggregates (compose, do not merge).
- In-crate Temporal workflow SDK.
- Graph storage — `id_effect_graph` orders projections only.

## Consequences

- Feature `es-entity` on `id_effect_events` replaces feature `postgres` for production paths.
- Legacy `PgSqlJournalBackend` and `POSTGRES_JOURNAL_DDL` exports are removed after migration (wave 6).
- Breaking semver on `id_effect_events` consumers still on the raw SQL journal.

## References

- [adr-sql-driver-choice.md](adr-sql-driver-choice.md)
- `.maestro/specs/fp-events.md`
- `.cursor/plans/es_entity_duroxide_graph.plan.md`
