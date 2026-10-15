---
# Copy to .maestro/specs/releases/<version>.md at release kickoff (PLAN-OF-PLANS §7.1).
title: id_effect <version> — <theme from PLAN-OF-PLANS §6>
slug: release-<major>-<minor>
mode: heavy
work_type: initiative
risk_class: high            # releases touch manifests + CI → high per .maestro/policies/risk.yaml
version: 1
scorecard:
  method: "1.0"             # METHOD.md version used for targets and exit
  baseline_version: "<previous>"
  target_E: <from METHOD §5.2 / scorecard.sh --targets>
  target_RT: <…>
  max_red_rows: <…>
  rows_in_scope: [<row ids whose target moves this release, incl. carry-over>]
  carry_over: [<row ids missed last release>]
dx_kpis: { K1: "<≤ N s>", K4: "100%", K5: "<≥ N%>" }
acceptance_criteria:
  - Every T0 epic slotted for this release in PLAN-OF-PLANS §6 is shipped.
  - Every epic in scope has its leaves shipped with evidence naming the scorecard rows it moves.
  - Blind re-score (METHOD §6) recorded in docs/scorecard/results/<version>.json.
  - Measured E ≥ target_E − 1.0, or each missed row has a written carry-over in REPORT-<version>.md.
  - No row regresses by 5 or more without an ADR note.
  - DX KPIs above measured by scripts/dx-kpis.sh and met.
  - QUALITY_BAR gates green (coverage ≥ 96 %), cargo-semver-checks clean except breaks listed in MIGRATION-<version>.md, bench budget gates green.
  - CHANGELOG has a "Scorecard" section with the index deltas.
non_goals:
  - <epics deliberately deferred, with target release>
---

# id_effect <version> — <theme>

## Scope selection (PLAN-OF-PLANS §7.1)

| Epic | Track | Rows moved (current → target) | value | effort | priority | In? |
|---|---|---|---|---|---|---|
| E0.x … | T0 | eco.license 8 → 98 | … | S | … | ✓ (T0 mandatory) |

## Waves
See `.maestro/missions/release-<major>-<minor>.execution.md`.

## Exit report
Filled in at exit: `docs/scorecard/REPORT-<version>.md`.
