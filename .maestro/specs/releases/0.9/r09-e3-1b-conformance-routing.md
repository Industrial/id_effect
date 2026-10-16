---
title: 'E3.1b — SQLite, MySQL, replica routing: r09-e3-1b-conformance-routing'
slug: r09-e3-1b-conformance-routing
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'One generic test module instantiated per backend (#6 evidence); `RoutingSqlClient{primary, replicas}` sends `query` to replicas and `execute`/`begin` to primary; test `reads_go_to_replica_writes_to_primary`. Book section in ch28-01.'
  - 'Gate: all backend features'
  - 'Owns: `crates/id_effect_sql/tests/conformance/**`, `src/routing.rs`'
---

# r09-e3-1b-conformance-routing

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E3.1b — SQLite, MySQL, replica routing (M, wave 2, after E3.1a) · data.backends #2 #3 #6 #7". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
