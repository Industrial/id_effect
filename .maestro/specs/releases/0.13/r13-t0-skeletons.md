---
title: 'T0 — ADR 0010 Amendment 9, crate moves and skeletons (ai, admin, i18n, Pipeline): r13-t0-skeletons'
slug: r13-t0-skeletons
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'docs/adrs/0010-crate-tiers.md Amendment 9 (for 0.13): id_effect_ai leaves the labs row and enters tier integration (moved with history: `git mv labs/id_effect_ai crates/id_effect_ai`; stays `publish = false` in the transitional merge tier, listed in `PENDING_PROMOTION` of scripts/validate-crate-packaging.sh, until r13-e9-1-promote); id_effect_admin and id_effect_i18n are new tier-integration crates; the id_effect_axum `ui` feature adds no crate; examples/reference/pipeline is tier example, publish = false, depending on id_effect_kit only; each crate moves to platform when the Pipeline depends on it (check P of scripts/tier-conformance.sh).'
  - 'Root Cargo.toml members gain crates/id_effect_ai, crates/id_effect_admin, crates/id_effect_i18n and examples/reference/pipeline; labs/Cargo.toml drops id_effect_ai; `examples/recipes` is added to the root `exclude`; crates/id_effect_cli/templates/admin-stub/** is deleted (its only references are scorecard evidence text, re-scored at exit).'
  - '`bash scripts/validate-crate-packaging.sh --print-order` lists id_effect_i18n after id_effect and id_effect_axum, id_effect_ai after id_effect_platform and id_effect_config, id_effect_admin after id_effect_sql, id_effect_axum, id_effect_authz and id_effect_i18n, all three before id_effect_kit and id_effect_tool; crates.io availability of the three names is recorded in the amendment (generic User-Agent, D17).'
  - 'The moved ai crate builds and its existing tests pass unchanged (manifest path fixes only); admin, i18n and pipeline skeletons each have a README with a `rust` fence, one example, one test and `#![deny(missing_docs)]`; .github/workflows/ci.yml gains the `-p` lines and the labs job no longer builds ai; docs/MIGRATION-0.13.md exists with one heading per planned break.'
  - 'Tier: governance (ADR and manifests). Secure by default: skeleton crates set `#![forbid(unsafe_code)]` and declare no `dangerous_*` API.'
  - 'Gate: `bash scripts/validate-crate-packaging.sh`; `cargo nextest run -p id_effect_ai -p id_effect_admin -p id_effect_i18n -p pipeline`; `cargo nextest run --manifest-path labs/Cargo.toml --workspace`; `bash scripts/tier-conformance.sh` (no new FAIL)'
  - 'Owns: `docs/adrs/0010-crate-tiers.md` (Amendment 9), root `Cargo.toml` (members, exclude), `labs/Cargo.toml`, `crates/id_effect_ai/**` (move and manifest paths only), `crates/id_effect_admin/` (skeleton), `crates/id_effect_i18n/` (skeleton), `examples/reference/pipeline/` (skeleton), `crates/id_effect_cli/templates/admin-stub/**` (deleted), `scripts/validate-crate-packaging.sh`, `.github/workflows/ci.yml` (`-p` lines, labs job), `docs/MIGRATION-0.13.md` (headings)'
---

# r13-t0-skeletons

Leaf of `.maestro/specs/releases/0.13.0.md`, section "T0". The release spec's row is binding; this
file exists so Maestro can track the leaf as its own task.

Wave 0 head. Blocked by: the v0.12.0 tag merged into `release/0.13.0`. Blocks every wave-1 breadth
leaf. Runs beside r13-t0-adr0015, r13-f-rcgen (which edits other manifests' rcgen lines only) and
r13-f-any-env.
