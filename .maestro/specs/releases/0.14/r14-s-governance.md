---
title: 'E13.1 Sweep — published release schedule, public funding channel, verified security-release history: r14-s-governance'
slug: r14-s-governance
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'eco.stability#4 (row eco.stability 75 → 88; #1 ">= 1.0" stays open): a published, predictable release schedule. Today GOVERNANCE.md "## Releases" (:60-77) describes the release process against PLAN-OF-PLANS but gives no dates, and the tags came out irregularly (v0.6.0 2026-10-14 to v0.13.0 2026-10-25, v0.4.0 → v0.6.0 three months). New docs/RELEASES.md: the cadence, a dated table `| Version | Planned | Released | Note |` from 0.14.0 to 1.0.0 (themes from PLAN-OF-PLANS §6), the rule for a slip (a dated note in the row, never a silent move), patch and security releases out of band (SECURITY.md), and the support window it implies (SECURITY.md:45-50); GOVERNANCE.md "## Releases" and README.md "### Releases" (:221) link it. `scripts/check-release-schedule.sh` checks the table mechanically: planned dates ISO and ascending, every `Released` date equal to its `vX.Y.Z` tag''s date (`git tag --format=%(creatordate:short)`), and the next unreleased row''s planned date not in the past unless that row has a note.'
  - 'eco.backing#8 (row eco.backing 38 → 50 once the account exists; #1 #2 #3 #7 need people): a public funding channel. New .github/FUNDING.yml (GitHub Sponsors, plus `custom:` only if the owner names one) and a README.md section "## Sponsoring" (what funding pays for, link to the Sponsors page, link to GOVERNANCE.md). Creating and enrolling the Sponsors account is an owner step (release spec D7); this leaf writes the files and records the step in CHANGELOG "Owner steps". The check counts only once the button resolves.'
  - 'sec.supply#7 (row sec.supply 88 → 100 once the owner publishes; the 0.13 carry-over): verification only. The KPI already exists: scripts/security-kpis.sh `kpi_security_releases` (:948) compares SECURITY.md "## Security releases" (:67-99, 0.10.1 with 12 rows `pending`, H2 `not a published crate`) with `repos/Industrial/id_effect/security-advisories?state=published`, with self-test cases at :1457-1498. Publishing the 0.10.1 GHSAs is an owner step (D7). This leaf (a) adds `--only <kpi>` to the script so the one KPI runs without the fuzz / bench / compile-fail work (`configure_release` :263 knows 0.11-0.13 only; the 0.14 case is r14-x-kpis''s), (b) once the owner has published, replaces each `pending` cell with the GHSA id and link and updates the "pending" note under the table, and (c) records the result: `pending` (nothing published, the leaf is then done with the row unmoved and the owner step open) or `pass`.'
  - 'Tests: `bash scripts/check-release-schedule.sh --self-test` (fixture cases `dates_ascending`, `released_matches_tag_date`, `released_date_differs_from_tag_fails`, `overdue_row_without_note_fails`, `overdue_row_with_note_passes`); `bash scripts/security-kpis.sh --self-test` with a new case `only_runs_the_named_kpi` (and every existing case unchanged); `bash scripts/security-kpis.sh --only security_releases --out .tmp/r14/security-releases.json 0.13.0` reports `pending` before and `pass` after the owner publishes; actionlint / a YAML parse of .github/FUNDING.yml.'
  - 'Book / docs: docs only (no book chapter: METHOD process rows take the document as evidence, as GOVERNANCE.md and SECURITY.md are for eco.backing#4-6 and sec.supply#4,#8). Tier: no crate touched. Secure by default: no new surface; the GHSA cells only link published advisories, never private drafts.'
  - 'Coverage: not applicable (no Rust code).'
  - 'Gate: `bash scripts/check-release-schedule.sh --self-test && bash scripts/check-release-schedule.sh`; `bash scripts/security-kpis.sh --self-test`; `bash scripts/security-kpis.sh --only security_releases --out .tmp/r14/security-releases.json 0.13.0` (exit 0; `pending` or `pass`, never FAIL); `bash scripts/check-workflows.sh`'
  - 'Owns: `docs/RELEASES.md` (new), `scripts/check-release-schedule.sh` (new), `.github/FUNDING.yml` (new), `GOVERNANCE.md` (section "## Releases": the schedule paragraph and link), `README.md` (the "### Releases" link and the new "## Sponsoring" section), `SECURITY.md` (section "## Security releases" only: Advisory cells and the note under the 0.10.1 table), `scripts/security-kpis.sh` (the `--only` option: usage header, argument parsing near :1546, the dispatch at :1061, one self-test case), append-only: `CHANGELOG.md` (incl. "Owner steps")'
---

# r14-s-governance

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r14-t0-adr0016 (the release targets are stored). Blocks r14-x-kpis (wave 6: adds
the 0.14 `configure_release` case on top of this leaf's `--only`) and r14-x-rescore. Two of its three
checks only count after owner steps (D7: publish the 0.10.1 GHSAs; create the Sponsors account); the
leaf is done when the files and checks are in place, whatever the owner has done, and it says which.

Defaults the owner may overturn: (1) cadence: one minor release every four weeks, tagged on a Tuesday,
first date 0.14.0 at the planned 0.14 exit; this is an assumption written to be overturned, since the
history so far is irregular. (2) Funding: GitHub Sponsors on the `Industrial` account (the repository
owner); the owner may name a personal account or Open Collective instead. (3) The schedule check is a
script run at the exit gate, not a CI job (no ci.yml edit in wave 1).
