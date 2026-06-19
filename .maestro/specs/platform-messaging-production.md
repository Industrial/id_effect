---
title: Platform Messaging Production
slug: platform-messaging-production
mode: heavy
work_type: initiative
risk_class: medium
version: 1
acceptance_criteria:
  - "id_effect_sql_pg on sqlx 0.8; deadpool/tokio-postgres removed"
  - "id_effect_jobs: Apalis, obix outbox/inbox, rdkafka; memory feature for tests"
  - "id_effect_events PgSqlJournalBackend on shared PgPoolKey"
  - "Part VI ch31 documents production stack"
  - "devenv postgres + DATABASE_URL for integration tests"
non_goals:
  - Backward-compatible deadpool/sqlx dual stack
  - v2 crates or ADR v2 files
---

# Platform Messaging Production

Breaking replacement of the SQL stack and job/outbox stubs with production adapters (sqlx, Apalis, obix, rdkafka).
