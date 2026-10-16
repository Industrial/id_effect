---
title: 'E3.1b — SQLite, MySQL, replica routing: r09-e3-1b-sqlite'
slug: r09-e3-1b-sqlite
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Same conformance suite as pg runs green on SQLite in-memory and file DBs.'
  - 'Gate: `cargo nextest run -p id_effect_sql --features sqlite`'
  - 'Owns: `crates/id_effect_sql/src/sqlite/**`, feature `sqlite`'
---

# r09-e3-1b-sqlite

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E3.1b — SQLite, MySQL, replica routing (M, wave 2, after E3.1a) · data.backends #2 #3 #6 #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
