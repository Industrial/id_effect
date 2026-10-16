---
title: 'E6.5 part 1 — `id_effect_test`: r09-e6-5-isolation-fixtures'
slug: r09-e6-5-isolation-fixtures
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - '`isolated_db()` per test: transaction rolled back at end, or template-database clone for tests that commit (#3); `Fixture`/factory trait with `FromRow` + Schema-arbitrary defaults (#4); re-export of the in-process HTTP client (#1). Book `part4` integration-testing chapter, example.'
  - 'Gate: same + tier-conformance id_effect_test'
  - 'Owns: `crates/id_effect_test/src/{db.rs,fixtures.rs,http.rs}`'
---

# r09-e6-5-isolation-fixtures

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E6.5 part 1 — `id_effect_test` (M, wave 2) · test.integration #2 #3 #4". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
