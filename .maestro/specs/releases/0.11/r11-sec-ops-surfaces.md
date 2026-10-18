---
title: 'E11.2 — Console, /docs and /metrics locked down in prod: r11-sec-ops-surfaces'
slug: r11-sec-ops-surfaces
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'M7: DEFAULT_SCALAR_URL (crates/id_effect_axum/src/openapi/ui.rs:69, @1) pinned to an exact version with its SRI sha384 integrity set by default (:103 is None today); ApiDocs is not mounted under the prod profile unless guarded or explicitly public_dangerous() (posture).'
  - 'M6: the service template''s local_operator shortcut (templates/service/src/console.rs.template:71-79) exists only in the dev profile; the console checks Host and Origin against console.allowed_hosts (default: the bind address) and answers 403 otherwise; outside dev the console mounts only behind a non-loopback guard; the Ledger enables the console in dev.toml only (base.toml:72-73 sets enabled = true for every profile today).'
  - 'M4 (template): BodyLimitLayer and TimeoutLayer wrap SecurityLayer (templates/service/src/lib.rs.template:134-137). L2 (metrics): the template serves /metrics on the console listener or behind a guard, not on the public router (:131); id_effect_axum metrics gains a guarded router helper.'
  - 'Regression tests `m7_scalar_bundle_pinned_with_integrity`, `m7_docs_disabled_in_prod_unless_guarded`, `m6_console_rejects_foreign_host_and_origin`, `m6_loopback_operator_only_in_dev`, `m4_template_limit_layers_wrap_security_layer`, `l2_template_metrics_not_public` (`m6_ledger_console_disabled_in_prod_profile` lives in r11-w3-ledger).'
  - 'Gate: `cargo nextest run -p id_effect_axum -p id_effect_console -p id_effect_tool --all-features`; `bash scripts/template-smoke.sh`'
  - 'Owns: `crates/id_effect_axum/src/{openapi/ui.rs,openapi/mod.rs,metrics.rs}`, `crates/id_effect_axum/tests/openapi_ui.rs`, `crates/id_effect_console/src/{api.rs,lib.rs}`, `crates/id_effect_console/tests/host_origin.rs`, `crates/id_effect_tool/templates/service/{src/console.rs.template,src/lib.rs.template,config/**}`, `crates/id_effect_tool/tests/template_security.rs`, `examples/reference/ledger/config/{base.toml,dev.toml,prod.toml}`'
---

# r11-sec-ops-surfaces

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E11.2 — Secure by default" (M4 template, M6,
M7, L2 metrics). The release spec's row is binding; this file exists so Maestro can track the leaf
as its own task.

Wave 2. Blocked by: r11-sec-h8-routing (console and template route call sites), r11-sec-csrf-headers
and r11-sec-limits (layers the template wires). r11-w3-kit-template owns the templates next.
