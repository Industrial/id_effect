---
title: 0.8 W3-A — id_effect_kit facade crate; new-crate registration (kit + tool)
slug: r08-w3-a-kit
mode: light
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'docs/adrs/0010-crate-tiers.md Amendment 4 names id_effect_kit (integration tier; moves to platform once examples/reference exists) and id_effect_tool (integration tier, publishes the id-effect binary), per ADR 0010 Rule 5 and ADR 0012.'
  - 'crates/id_effect_kit: version.workspace, readme, tier = "integration", #![deny(missing_docs)], forbid(unsafe_code); features web (id_effect_axum), sql-pg (id_effect_sql/pg), otel (id_effect_opentelemetry), cluster (id_effect_node), full; a prelude (id_effect core items used across examples, plus per-feature items: EffectExecutor, execute, serve_until_shutdown, SecurityLayer, health/readiness routers, metrics_router, SqlClientService, SqlError, with_transaction, OtelConfig, install_from_config, run_main, RunMainConfig) and whole-crate re-exports (kit::axum, kit::sql, ...); examples/010_prelude.rs runs; README with a rust block; tests satisfy tier-conformance density.'
  - 'Registration: root Cargo.toml members, .moon/workspace.yml + crate moon.yml, scripts/validate-crate-packaging.sh LEVEL/ORDER for id_effect_kit (last level, after all its deps) and a reserved slot for id_effect_tool after it; the ci.yml semver job excludes both new crates until they have a published baseline.'
  - 'bash scripts/tier-conformance.sh id_effect_kit PASS; bash scripts/validate-crate-packaging.sh PASS and --print-order lists id_effect_kit last (id_effect_tool added by W3-B); cargo hack-style check of each feature alone and full (cargo check -p id_effect_kit --no-default-features --features X for X in web, sql-pg, otel, cluster, full); clippy -D warnings, rustdoc -D warnings.'
non_goals:
  - The id_effect_tool crate itself (W3-B).
  - Moving the kit to the platform tier (needs examples/reference, 0.9).
---

# 0.8 W3-A — id_effect_kit

L3/L11; E1.2. Release spec `.maestro/specs/releases/0.8.0.md`.
