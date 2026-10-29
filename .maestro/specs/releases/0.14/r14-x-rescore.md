---
title: 'Proof (exit) — Blind re-score of 0.14.0, results, report and target check: r14-x-rescore'
slug: r14-x-rescore
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Blind re-score per docs/scorecard/METHOD.md §6 (:224-238) by an assessor (agent or person) who built none of the 0.14 leaves and has not seen the 0.14.0 targets, working from the repository at the release candidate commit, the 0.13.0 `evidence` strings and the r14-x-level-audit evidence map; per row the assessor records level and met checks (`rows[].v2.level` / `met` for 0.14.0) with evidence per met check (`rows[].v2.evidence`); every row whose blind score differs from the expectation by more than 10 points is adjudicated in REPORT-0.14.0.md (§6 step 4). D3 is applied as written: a row is `production` only if the audit map cites a reference-app CI test for it.'
  - 'docs/scorecard/scorecard.json gains the measured 0.14.0 column only (no target edits); `bash scripts/scorecard.sh --json 0.14.0 --write` stores docs/scorecard/results/v0.14.0.json; `--report 0.14.0` writes docs/scorecard/REPORT-0.14.0.md (indices, deltas against 0.13.0 and the targets, changed rows with evidence, regressions, misses with carry-over; METHOD §6 step 6); `--html` refreshes docs/scorecard/index.html.'
  - '`bash scripts/scorecard.sh --compare-targets 0.14.0` passes (measured E ≥ planned E − 1.0 on the plan stored by r14-t0-adr0016) or each missed row has a written carry-over to 0.15 in the report and in PLAN-OF-PLANS §11; `--diff 0.13.0 0.14.0` passes (no drop of 5 or more without an ADR note).'
  - 'PLAN-OF-PLANS §11 gains the 0.14 exit row (measured E/RT/red, the rows promoted to production and the reference-app test each rests on, misses, carry-overs) and the §6 row 0.14.0 exit KPI ("Warden reference app green; every promoted row cites a reference-app CI test", PLAN-OF-PLANS.md:356) is marked met or missed.'
  - 'Tier: docs. Secure by default: the report has a Security section from docs/security/kpis-0.14.json (including whether sec.supply#7 is met by a published GHSA).'
  - 'Coverage: not applicable (data and docs).'
  - 'Gate: `bash scripts/scorecard.sh --validate`; `bash scripts/scorecard.sh --compare-targets 0.14.0`; `bash scripts/scorecard.sh --diff 0.13.0 0.14.0`; `bash scripts/scorecard.sh --validate --self-test`'
  - 'Owns: `docs/scorecard/scorecard.json` (0.14.0 measured column only), `docs/scorecard/results/v0.14.0.json`, `docs/scorecard/REPORT-0.14.0.md`, `docs/scorecard/index.html`, `.maestro/specs/roadmap/PLAN-OF-PLANS.md` (§11 exit row and the §6 0.14.0 KPI mark)'
---

# r14-x-rescore

Leaf of `.maestro/specs/releases/0.14.0.md`, section "Proof" (exit). The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Wave 7 (exit). Blocked by: r14-x-level-audit and r14-x-kpis. Runs beside r14-t0-migration-guide
(disjoint: that leaf owns CHANGELOG.md and copies the Scorecard numbers from REPORT-0.14.0.md once
this leaf has written it, so it merges second).

Default the owner may overturn: the assessor scores the release candidate commit, as 0.13 did,
not the tag (METHOD §6 step 1 says the tag; the tag does not exist until the owner pushes it, so the
report names the commit and the tag is checked to point at it).
