---
title: 'Exit — Blind re-score of 0.13.0, results, report and index check: r13-x-rescore'
slug: r13-x-rescore
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Blind re-score per docs/scorecard/METHOD.md §6 by an assessor (agent or person) who built none of the 0.13 leaves, working from the repository at the release candidate commit and the r13-w8-level-audit evidence map; per row the assessor records level and met checks (`rows[].v2.level` / `met` for 0.13.0); adjudications between assessor and evidence map are written in REPORT-0.13.0.md.'
  - 'docs/scorecard/scorecard.json gains the measured 0.13.0 column only (no target edits, D13); `bash scripts/scorecard.sh --json 0.13.0 --write` stores docs/scorecard/results/v0.13.0.json; `--report 0.13.0` writes REPORT-0.13.0.md; `--html` refreshes index.html.'
  - '`bash scripts/scorecard.sh --compare-targets 0.13.0` passes (measured E ≥ planned E − 1.0 = 83.3 on the stored plan, or the plan restated by r13-t0-adr0015) or each missed row has a written carry-over to 0.14 in the report and in PLAN-OF-PLANS §11; `--diff 0.12.0 0.13.0` passes (no drop of 5 or more without an ADR note).'
  - 'PLAN-OF-PLANS §11 gains the 0.13 exit row (measured E/RT/red, misses, carry-overs).'
  - 'Tier: docs. Secure by default: the report has a Security section (from docs/security/kpis-0.13.json).'
  - 'Gate: `bash scripts/scorecard.sh --validate`; `bash scripts/scorecard.sh --compare-targets 0.13.0`; `bash scripts/scorecard.sh --diff 0.12.0 0.13.0`; `bash scripts/scorecard.sh --validate --self-test`'
  - 'Owns: `docs/scorecard/scorecard.json` (0.13.0 measured column only), `docs/scorecard/results/v0.13.0.json`, `docs/scorecard/REPORT-0.13.0.md`, `docs/scorecard/index.html`, `.maestro/specs/roadmap/PLAN-OF-PLANS.md` (§11 exit row)'
---

# r13-x-rescore

Leaf of `.maestro/specs/releases/0.13.0.md`, section "Proof" (exit). The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.

Exit. Blocked by: r13-w8-level-audit and r13-w8-kpis. Runs beside r13-t0-migration-guide.
