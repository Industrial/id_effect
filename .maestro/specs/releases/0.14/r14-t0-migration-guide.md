---
title: 'T0 (exit) — MIGRATION-0.14, the CHANGELOG 0.14 Scorecard and Security sections, the owner steps: r14-t0-migration-guide'
slug: r14-t0-migration-guide
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'docs/MIGRATION-0.14.md has one section per finding of `cargo semver-checks --workspace --baseline-rev v0.13.0` (the command of the ci.yml `semver` job, :950-955), each with its one-minor deprecation shim (L9) or its rewrite, in the shape of docs/MIGRATION-0.13.md (a "## <crate>: <change>" heading per break, before/after code); the preamble and headings r14-t0-skeletons created, and the sections the 0.14 leaves appended during the release, are kept and checked against the tool output; non-semver changes users notice are listed too: id_effect_kafka, id_effect_amqp and id_effect_nats first published, the Ledger binary''s subcommands (r14-p-toolchain; `serve` is the default, so `ledger` alone behaves as in 0.13), the Relay contract additions (r14-p-relay, additive), and every new `dangerous_*` opt-out.'
  - 'CHANGELOG.md has the 0.14.0 section in the shape of the 0.13.0 one (CHANGELOG.md:3-675): Added, Changed, Deprecated (each shim and its removal release), Documentation, Security, Performance, Scorecard, Owner steps. Scorecard: planned (`bash scripts/scorecard.sh --targets 0.14.0`) against measured (REPORT-0.14.0.md of r14-x-rescore) for the indices and for every row with a 0.14.0 target or a promotion, with the reference-app test each promotion rests on; DX KPIs from docs/benchmarks/dx-kpis-0.14.json; bench gates from docs/benchmarks/v0.14.json. Security: every Security-table rule of 0.14.0.md, new `dangerous_*` opt-outs, fuzz (≥ 29) and compile-fail (≥ 23) counts, from docs/security/kpis-0.14.json.'
  - 'Every leaf CHANGELOG line is present; every follow-up carried into 0.14 (F53, F59, F62, F63/F65, F64, F66, sec.supply#7; CHANGELOG.md:651-658) is either closed by its leaf or named as carried to 0.15.'
  - 'Owner steps subsection (not done by the release automation): accept ADR 0016 (docs/adrs/0016-sweep-promote-warden.md, written by r14-t0-adr0016); publish the GitHub Security Advisories for the "pending" rows of SECURITY.md "Security releases" (sec.supply#7; each row gets its GHSA id); the first crates.io publication of id_effect_kafka, id_effect_amqp and id_effect_nats, which publish.yml performs from the tag through `scripts/validate-crate-packaging.sh --print-order` (r14-t0-skeletons adds the three crates to its `ORDER` list, scripts/validate-crate-packaging.sh:86-97 at HEAD; this leaf checks they are there; crates.io creates them on first publish); push the `v0.14.0` tag on the merge commit (`git tag -a v0.14.0 -m "v0.14.0" && git push origin v0.14.0`); still open from earlier releases: accept ADR 0013, the OpenSSF Best Practices badge, `sudo tc` on the bench runner.'
  - 'Tier: documentation. Secure by default: every new `dangerous_*` opt-out is listed with the finding it reopens (from `bash scripts/check-dangerous.sh`).'
  - 'Coverage: not applicable (documentation).'
  - 'Gate: `cargo semver-checks --workspace --baseline-rev v0.13.0` output is a subset of the MIGRATION-0.14 list; `bash scripts/check-dangerous.sh`; `bash scripts/validate-crate-packaging.sh --print-order | grep -x id_effect_kafka && bash scripts/validate-crate-packaging.sh --print-order | grep -x id_effect_amqp && bash scripts/validate-crate-packaging.sh --print-order | grep -x id_effect_nats`; `bash scripts/check-version-drift.sh`'
  - 'Owns: `docs/MIGRATION-0.14.md`, `CHANGELOG.md`'
---

# r14-t0-migration-guide

Leaf of `.maestro/specs/releases/0.14.0.md`, section "T0" (exit). The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 7 (exit). Blocked by: every other leaf; merges after r14-x-rescore (it copies the Scorecard
numbers from REPORT-0.14.0.md) and r14-x-kpis. Runs beside r14-x-rescore with disjoint files.

Defaults the owner may overturn: the leaf does not perform any owner step; it records them. New
crates have no semver baseline, so `cargo semver-checks` reports nothing for id_effect_kafka,
id_effect_amqp and id_effect_nats; their first publication is named in the guide instead.
