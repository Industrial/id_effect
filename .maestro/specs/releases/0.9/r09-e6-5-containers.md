---
title: 'E6.5 part 1 — `id_effect_test`: r09-e6-5-containers'
slug: r09-e6-5-containers
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Effect-native `Postgres/MySql/Redis/Keycloak/Vault` containers as scoped Layers (testcontainers), torn down on scope exit or interrupt (#2).'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_test`'
  - 'Owns: `crates/id_effect_test/src/containers/**`'
---

# r09-e6-5-containers

Leaf of `.maestro/specs/releases/0.9.0.md`, section "E6.5 part 1 — `id_effect_test` (M, wave 2) · test.integration #2 #3 #4". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
