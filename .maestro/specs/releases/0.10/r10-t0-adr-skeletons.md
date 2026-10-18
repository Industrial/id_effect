---
title: 'T0 — governance: r10-t0-adr-skeletons'
slug: r10-t0-adr-skeletons
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Amendment 6 names the six crates + tier (D5); each skeleton has README with a `rust` fence, one example, `#![deny(missing_docs)]`, one test, tier `integration`; `--print-order` lists `id_effect_events`, `id_effect_messaging`, `id_effect_storage` after `id_effect_sql`/`id_effect_platform`, and `id_effect_jobs` (after `id_effect_node`), `id_effect_mail` (after jobs, auth), `id_effect_console` (after jobs, authz) before `id_effect_kit`; crates.io availability of the six names recorded in the leaf evidence; `sec.authn.targets["0.10.0"] == 88`.'
  - 'Gate: `bash scripts/tier-conformance.sh id_effect_jobs id_effect_messaging id_effect_mail id_effect_console id_effect_storage` PASS; `bash scripts/validate-crate-packaging.sh`; `bash scripts/scorecard.sh --targets 0.10.0` prints E 74.1'
  - 'Owns: `docs/adrs/0010-crate-tiers.md` (Amendment 6), root `Cargo.toml` members, `crates/id_effect_{jobs,messaging,mail,console,storage}/` skeletons, `scripts/validate-crate-packaging.sh` (`LEVEL`, `ORDER`), `.github/workflows/ci.yml` (`-p` for the new crates in the pg job at `:203` and the containers job at `:229`), `deny.toml`, `docs/scorecard/scorecard.json` (D6 target line only)'
---

# r10-t0-adr-skeletons

Leaf of `.maestro/specs/releases/0.10.0.md`, section "T0 — governance (S, wave 1 first + exit)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
