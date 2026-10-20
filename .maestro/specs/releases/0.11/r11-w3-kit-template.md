---
title: 'Wave 3 — Kit features, prod posture check, templates: r11-w3-kit-template'
slug: r11-w3-kit-template
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'id_effect_kit features workflow (id_effect_workflow), mesh-mtls and kubernetes; full updated; the prelude exposes id_effect::stm.'
  - 'ADR 0013 R10: id_effect_kit::posture::check(profile) returns a PostureReport with named probes — console guarded, docs closed, mesh transport encrypted, redaction policy non-empty, SSRF guard on, CSRF on, load shedding on, no memory-only security state, and no recorded dangerous opt-out that is not listed in config `security.accepted_risks` with a reason; under the prod profile startup fails on any failed probe; dev and test profiles report only.'
  - 'The service, worker and cluster templates call the posture check at startup; the cluster template configures mTLS with PEM paths (dev certificates for local runs); the service template passes the prod posture check as generated.'
  - 'scripts/check-dangerous.sh proves every `pub fn dangerous_*` / `insecure_*` under crates/ calls id_effect::observability::posture::record_dangerous, with --self-test; added as one step to .github/workflows/lint.yml.'
  - 'Tests `posture_prod_fails_on_unaccepted_opt_out`, `posture_accepted_risk_needs_reason`, `posture_report_lists_probes`, `template_service_passes_prod_posture`.'
  - 'Gate: `cargo nextest run -p id_effect_kit --all-features`; `bash scripts/template-smoke.sh`; `bash scripts/check-dangerous.sh --self-test && bash scripts/check-dangerous.sh`; `DX_KPIS_RELEASE=0.11 bash scripts/dx-kpis.sh` K2 ≤ 8 s'
  - 'Owns: `crates/id_effect_kit/**` (incl. new `src/posture.rs`, `tests/posture.rs`), `crates/id_effect_tool/templates/**`, `scripts/check-dangerous.sh`, one step in `.github/workflows/lint.yml`'
---

# r11-w3-kit-template

Leaf of `.maestro/specs/releases/0.11.0.md`, section "Wave 3 — integration and proof". The release
spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: every r11-sec-* leaf, r11-e8-3-tcp-tls, r11-e7-5-activities-timers (kit feature),
r11-e1-9-dylint (lint.yml). r11-w3-ledger and r11-e11-4-compile-fail follow.
