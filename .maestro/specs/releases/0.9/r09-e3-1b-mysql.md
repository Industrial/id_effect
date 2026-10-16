---
title: 'E3.1b — SQLite, MySQL, replica routing: r09-e3-1b-mysql'
slug: r09-e3-1b-mysql
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Conformance suite green on MySQL 8 (testcontainers).'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_sql --features mysql`'
  - 'Owns: `crates/id_effect_sql/src/mysql/**`, feature `mysql`'
---

# r09-e3-1b-mysql

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E3.1b — SQLite, MySQL, replica routing (M, wave 2, after E3.1a) · data.backends #2 #3 #6 #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
