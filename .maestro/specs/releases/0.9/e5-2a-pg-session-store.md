---
title: "E5.2a follow-up: PgSessionStore"
slug: r09-e5-2a-pg-session-store
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'id_effect_auth feature `pg`: `PgSessionStore` implements `SessionStore` (save as upsert, load, remove, remove_user, purge_expired) over `id_effect_sql::SqlClient` with `SqlValue` params.'
  - 'The table stores SHA-256 of the session id, never the raw id; ships a migration (`migrations/` in the crate, usable with id_effect_sql::migrate) creating `id_effect_sessions` with an index on user_id and on absolute_expires_at.'
  - 'Live Postgres test (ID_EFFECT_REQUIRE_PG=1) runs the same store behaviour suite as MemorySessionStore; book ch30-03 documents the store.'
  - 'Completes sec.authn #2 (persistent session store).'
---

# PgSessionStore

Follow-up recorded in `.maestro/missions/release-0-9.execution.md`, unblocked by
`r09-e3-1a-values-rows` (SqlValue) and `r09-e5-2a-credentials-jwt` (crate free).
