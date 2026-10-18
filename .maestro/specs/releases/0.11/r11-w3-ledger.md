---
title: 'Wave 3 — Ledger: prod posture, declared routes, durable account closure: r11-w3-ledger'
slug: r11-w3-ledger
mode: heavy
work_type: spec-slice
risk_class: high
version: 1
acceptance_criteria:
  - 'The Ledger (built only through id_effect_kit) passes the prod posture check with zero accepted risks; every route is public or guarded and an access-report snapshot test lists the public routes; the console is off in prod and on in dev behind its guard; the SendGrid host is allowlisted in the SSRF policy; no RawSqlCapability is provided; `cargo dylint` with raw_sql_non_literal is clean on the Ledger.'
  - 'Account closure runs as a durable workflow: an approval signal, a 30-day grace timer (under the test clock), a statement-export activity on the job queue and saga compensation; this makes the Ledger the reference-app consumer of id_effect_workflow, whose tier moves to platform (check P).'
  - 'Tests `ledger_prod_posture_is_clean`, `ledger_access_report_snapshot`, `h8_ledger_unregistered_route_denied`, `m6_ledger_console_disabled_in_prod_profile`, `account_closure_workflow_survives_restart`; the reference-ledger CI job runs them with Postgres, Redis, Mailpit and the S3 container.'
  - 'Gate: `ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p ledger` (Docker and Postgres required); `bash scripts/tier-conformance.sh` (all PASS)'
  - 'Owns: `examples/reference/ledger/**`, `.github/workflows/ci.yml` (reference-ledger job), the tier line in `crates/id_effect_workflow/Cargo.toml`'
---

# r11-w3-ledger

Leaf of `.maestro/specs/releases/0.11.0.md`, section "Wave 3 — integration and proof" (Ledger KPI).
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 3. Blocked by: r11-w3-kit-template and r11-e7-5-sagas-promote. Open question 4 in the release
spec asks whether the workflow promotion stays in scope; without it, drop the account-closure item
and keep id_effect_workflow at tier integration.
