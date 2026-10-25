---
title: 'Wave 7 — Pipeline reference app, back office: generated admin, htmx review queue, en/de, prod posture: r13-w7-pipeline-backoffice'
slug: r13-w7-pipeline-backoffice
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'D1 stage 5: the admin for `Source`, `Document` and `Label` is produced with `id-effect generate admin <Entity>` (the exact commands and every manual edit after generation are listed in the Pipeline README), with roles `reviewer` (list, view, update labels, bulk re-run extraction) and `admin` (all); label picker over the N:M relation; every change audited.'
  - 'An htmx + maud review queue (layout, components, partial updates, form errors bound to fields) in English and German: Accept-Language negotiation plus a per-user override; validation errors translated; locales under examples/reference/pipeline/locales/.'
  - 'Prod posture clean with zero accepted risks (admin guarded, MCP guarded, prompts not logged, tenant not from header, plus the 0.11/0.12 probes); id_effect_admin tier line moves to `platform` (check P).'
  - 'Tests `pipeline_admin_crud_and_audit`, `pipeline_admin_denied_without_role`, `pipeline_admin_bulk_rerun_extraction`, `pipeline_admin_label_picker_tenant_scoped`, `pipeline_ui_renders_de_and_en`, `pipeline_validation_errors_translated`, `pipeline_htmx_post_requires_csrf`, `pipeline_prod_posture_is_clean` (HTML asserted through the id_effect_test in-process HTTP client; no browser).'
  - 'Tier: example. Secure by default: as the posture test above; no `dangerous_*` opt-out taken.'
  - 'Gate: `ID_EFFECT_REQUIRE_PG=1 cargo nextest run -p pipeline`; CI job `reference-pipeline`; `bash scripts/tier-conformance.sh` (all PASS)'
  - 'Owns: `examples/reference/pipeline/src/{admin,ui}/**`, `examples/reference/pipeline/locales/**`, `examples/reference/pipeline/tests/{admin.rs,ui.rs,i18n.rs,posture.rs}`, the `reference-pipeline` job steps of `.github/workflows/ci.yml`, the tier line of `crates/id_effect_admin/Cargo.toml`; one `mod` line each in `src/lib.rs` and the README sections "Back office" and "Locales"'
---

# r13-w7-pipeline-backoffice

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof" (the §6 exit KPI, second half). The
release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 7, critical path. Blocked by: r13-w6-pipeline. Blocks r13-w8-level-audit and r13-w8-kpis.
