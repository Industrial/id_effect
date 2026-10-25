---
title: 'Wave 5 — Kit features and posture probes for the breadth batteries; templates; F55: r13-w5-kit-template'
slug: r13-w5-kit-template
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Kit features `ai`, `mcp`, `embeddings`, `admin`, `ui`, `i18n`, `tenancy` in crates/id_effect_kit/Cargo.toml (all off by default, all in `full`), prelude items for each, and re-exports `id_effect_kit::{ai, admin, i18n, ui}`.'
  - '`tenancy` bridges `id_effect_authz::tenant::TenantResolver` to both `TenantScope` and `id_effect_sql::CurrentTenant` in one layer (D10), so sql and authz stay independent.'
  - 'F55: `id_effect_kit::process` re-exports id_effect_process; the `cluster` template uses it (no direct id_effect_process dependency in templates).'
  - 'The admin generator template import switches to `id_effect_kit::admin::` (the template variable left by r13-e1-3-generate-admin); scripts/template-smoke.sh runs `id-effect generate admin` on a generated `service` project and builds and tests it.'
  - 'Posture probes in crates/id_effect_kit/src/posture.rs: "admin guarded", "mcp guarded", "prompts not logged", "tenant not from header"; each probe fails the prod profile on violation unless its opt-out is accepted (ADR 0013 R7 R10); scripts/check-dangerous.sh lists `dangerous_log_prompts`, `dangerous_mcp_write_tools`, `dangerous_tenant_from_header`.'
  - 'Tests `kit_tenancy_bridge_scopes_sql_and_authz`, `posture_reports_breadth_probes`, `posture_prod_refuses_public_admin`, `posture_prod_refuses_unguarded_mcp`, `template_service_generate_admin_round_trip`, `kit_reexports_id_effect_process`.'
  - 'Tier: integration (kit, tool). Coverage: `cargo llvm-cov nextest -p id_effect_kit --all-features --summary-only --fail-under-regions 96` and the same for id_effect_tool.'
  - 'Gate: `cargo nextest run -p id_effect_kit -p id_effect_tool --all-features`; `bash scripts/template-smoke.sh`; `bash scripts/check-dangerous.sh` (K1 with default features is measured by r13-w8-kpis)'
  - 'Owns: `crates/id_effect_kit/**`, `crates/id_effect_tool/{templates/**,Cargo.toml}`, `scripts/{template-smoke.sh,check-dangerous.sh}`'
---

# r13-w5-kit-template

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 5, critical path. Blocked by: r13-e1-3-generate-admin, r13-e9-1-mcp, r13-e9-1-embeddings,
r13-e9-4-i18n-web, r13-e5-5-tenancy, r13-e3-3-hooks, r13-e9-5-views. Blocks r13-w6-pipeline and
r13-f-relay-host.
