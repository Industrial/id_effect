---
title: 0.8 W3-B — id_effect_tool with `id-effect new` and four templates tested in CI (E1.1, E6.3)
slug: r08-w3-b-tool-new
mode: light
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'New crate crates/id_effect_tool (integration tier per ADR 0010 Amendment 4; version.workspace, readme, deny(missing_docs), forbid(unsafe_code)) publishing the binary id-effect ([[bin]] name = "id-effect", thin src/bin over a tested lib). No dependency on id_effect_kit. Registered: workspace member, moon project + moon.yml, validate-crate-packaging LEVEL/ORDER after id_effect_kit; tier-conformance PASS.'
  - '`id-effect new <name> --template service|worker|cli|cluster [--dest DIR] [--db none|pg] [--otel|--no-otel] [--local-workspace PATH]` renders templates embedded in the binary (build.rs-generated include_str! table or explicit table with a test that it matches the template dir; {{name}}/{{crate_name}}/{{id_effect_version}} substitution; Cargo.toml.template and _github naming so cargo package keeps them), refuses a non-empty destination, and writes for every template: Cargo.toml depending on id_effect_kit (+ id_effect, since its macros expand to ::id_effect), src, tests, Dockerfile, compose.yaml, .github/workflows/ci.yml, .env.example, README. --local-workspace writes [patch.crates-io] paths to the local id_effect* crates.'
  - 'Templates: service = axum app with SecurityLayer::recommended, /health + /ready (HealthRegistry), /metrics + http_metrics_layer, OTel when --otel, SQL client + a migration when --db pg (default --db none so it boots with no services); worker = CronRegistry jobs with history; cli = run_main + completions (from examples/cli-minimal); cluster = id_effect_node mesh node. The orphaned crates/id_effect_cli/templates/deploy-kamal is moved into the tool (Dockerfile/Kamal) and removed from id_effect_cli.'
  - 'scripts/template-smoke.sh generates each template into a temp dir outside the repo with --local-workspace and runs cargo test there (and cargo build); a ci.yml `templates` job runs it as a matrix over the 4 templates and docker-builds the service template; job added to ci-pass.needs; actionlint clean. cargo package -p id_effect_tool --list includes every template file.'
  - 'Unit tests for rendering, substitution, refusal on non-empty dest, option handling; clippy -D warnings, rustdoc -D warnings; coverage: bin excluded via the coverage IGNORE mechanism with a justification, lib tested.'
non_goals:
  - doctor and layer graph (W3-C); generators beyond `new` (0.9).
---

# 0.8 W3-B — id_effect_tool and `id-effect new`

Rows dx.scaffold (#1, #5, #6, #7, #8), dx.deploy (level tested). Release spec `.maestro/specs/releases/0.8.0.md`.
