---
title: 'SQLT — id_effect_sql without sqlx `time`: r10-sqlt-mysql-query-ci'
slug: r10-sqlt-mysql-query-ci
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'MySQL `DATETIME`/`TIMESTAMP`/`DATE`/`TIME` decode from the binary protocol into jiff; the sea-query bridge uses `with-jiff` only; `cargo tree -p id_effect_kit --all-features -e features -i sqlx` shows no `feature "time"` and `cargo tree -p id_effect_sql --all-features -e normal -i time` finds no match (CI step fails otherwise); MIGRATION-0.10 section "Data: sqlx''s `time` feature is off again".'
  - 'Gate: `<docker> -p id_effect_sql --all-features` (MySQL conformance); the CI step'
  - 'Owns: `crates/id_effect_sql/src/mysql/value.rs`, `src/query/time_conv.rs`, `crates/id_effect_sql/Cargo.toml` (sqlx `time` `:78`, `time` dep `:68`, `query` feature `:36`, sea-query `with-time` `:93`), `.github/workflows/ci.yml` (one check step), `docs/MIGRATION-0.10.md` (section draft)'
---

# r10-sqlt-mysql-query-ci

Leaf of `.maestro/specs/releases/0.10.0.md`, section "SQLT — id_effect_sql without sqlx `time` (M, wave 1) · D3". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
