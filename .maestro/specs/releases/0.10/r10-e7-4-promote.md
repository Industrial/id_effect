---
title: 'E7.4 — Events, `id_effect_events`: r10-e7-4-promote'
slug: r10-e7-4-promote
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Moved to `crates/`; `id_effect_graph` and `es-entity` dependencies and the `es_entity/` module removed; `SqlJournal` ported to `SqlClient`/`SqlValue` (pg + sqlite); `append_expected(stream, expected_version, events)`; command/query handlers and projections return `Effect` (no `run_sync` in public paths); live tests fail under `ID_EFFECT_REQUIRE_PG`. Tests `expected_version_conflict`, `cqrs_dispatch_is_an_effect`.'
  - 'Gate: `<pg> -p id_effect_events --all-features`'
  - 'Owns: `labs/id_effect_events/**` → `crates/id_effect_events/**`, `labs/Cargo.toml`, root `Cargo.toml` member line'
---

# r10-e7-4-promote

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E7.4 — Events, `id_effect_events` (M, wave 1) · data.events #6 #8 (+#1–#5 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
