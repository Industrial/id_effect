---
title: 'E11.2 — RawSql capability and a safe Redis clear: r11-sec-sql-redis'
slug: r11-sec-sql-redis
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'M16 (0.11.0.md D9): the effect-level query and execute (crates/id_effect_sql/src/client.rs:195, :207, today impl Into<String>) take impl Into<Sql>, where Sql: From<&''static str>; string literals still compile and a String does not; query_raw and execute_raw take RawSql and need RawSqlCapability in R, provided only by RawSqlCapability::layer_dangerous() (records a posture opt-out; the capability has no public constructor). Internal callers that build SQL (repository, migrate, query builder) use a crate-private trusted constructor. The SqlClient trait''s &str methods stay as the driver boundary. MIGRATION-0.11 section "Sql/RawSql".'
  - 'M17: clear() on a Redis store whose namespace is empty (crates/id_effect_redis/src/client.rs:355-367 turns "" into the glob "*"; store.rs:90-91, :287, :423, :569) returns RedisError::EmptyNamespace; the explicit flush is clear_all_dangerous() (records a posture opt-out).'
  - 'Regression tests `m16_query_with_string_does_not_compile` (trybuild, crates/id_effect_sql/tests/ui), `m16_query_raw_requires_capability` (trybuild), `m16_literal_query_still_compiles`, `m17_clear_with_empty_namespace_is_an_error`, `m17_clear_only_deletes_namespace_keys` (Docker Redis).'
  - 'Gate: `cargo nextest run -p id_effect_sql --test ui`; `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p id_effect_sql --all-features` (Postgres required); `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_redis --all-features` (Docker required); `cargo build --workspace --all-targets`'
  - 'Owns: `crates/id_effect_sql/**` (incl. new `src/raw.rs` and `tests/ui/m16_*`), `crates/id_effect_redis/src/{store.rs,client.rs}`, `crates/id_effect_redis/tests/store_clear.rs`'
---

# r11-sec-sql-redis

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M16, M17). The
release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r11-sec-adr0013. A grep at planning time found no workspace caller passing a
String to the effect-level query/execute outside id_effect_sql (the Ledger calls the SqlClient trait
methods, e.g. examples/reference/ledger/src/db.rs:80); if the implementer finds one outside this
leaf's Owns, it is migrated by the owner of that file in its wave, not here. The E1.9 lint
`raw_sql_non_literal` covers the trait methods.
