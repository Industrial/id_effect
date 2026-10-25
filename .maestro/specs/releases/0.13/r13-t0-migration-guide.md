---
title: 'T0 (exit) — MIGRATION-0.13 and CHANGELOG: r13-t0-migration-guide'
slug: r13-t0-migration-guide
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/MIGRATION-0.13.md has one section per `cargo semver-checks` break, each with its shim or rewrite: `id_effect_sql::repository` split into a module tree (paths kept by `pub use`), `Entity` new associated items (all defaulted, so manual impls compile), `EventLog::head` (provided method), `WsConfig` control-frame cap, `Http3Options` early-data and congestion settings, `HttpTransport::with_header`/`with_bearer`, rcgen 0.14 in any public signature (if r13-f-rcgen found one), crates/id_effect_cli/templates/admin-stub removed, id_effect_ai first published from crates/ (was labs).'
  - 'CHANGELOG.md has the 0.13.0 section with Scorecard (measured vs planned per row) and Security (every Security-table rule, new dangerous_* opt-outs, fuzz and compile-fail counts) subsections.'
  - 'Every leaf CHANGELOG line is present; every follow-up in the release spec "0.12 follow-ups" table is either closed (leaf merged) or named as carried in the CHANGELOG.'
  - 'Tier: documentation. Secure by default: every new dangerous_* opt-out is listed with the finding it reopens (from `scripts/check-dangerous.sh`).'
  - 'Gate: `cargo semver-checks` output is a subset of the MIGRATION-0.13 list; `bash scripts/check-dangerous.sh`'
  - 'Owns: `docs/MIGRATION-0.13.md`, `CHANGELOG.md`'
---

# r13-t0-migration-guide

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T0" (exit). The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Exit. Blocked by: every other leaf. Runs beside r13-x-rescore.
