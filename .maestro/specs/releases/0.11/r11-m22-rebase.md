---
title: '§7.0b — Method 2.2 re-base: r11-m22-rebase'
slug: r11-m22-rebase
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Step 1 (non-goals leave the score, every framework): METHOD.md §3.3 rewritten — a `non-goal` status is set only by a locked decision; a non-goal row gets weight 0 and leaves its group mean, U, E, RT and the row KPIs; a non-goal check leaves its row checklist and the row scores clamp(1, cap(L), round(100·m/(n−k))). scorecard.json marks row `web.graphql` (`non_goal: {decision: "L15"}`) and checks `fault.reload#2`, `#6`, `#7`, `dx.compile#3` (L8), `web.ui#3`, `#4` (L7), `data.backends#4`, `#5` (L6). `conc.safety#8` and `obs.tracing#8` stay scored. scorecard.schema.json and `scripts/scorecard.sh --validate` accept the field and reject a non-goal without a decision id.'
  - 'Step 2 (.NET scope): frameworks[dotnet].scope adds Orleans and the Durable Task Framework as first-party Microsoft; every .NET cell that alignment-2.0.json classifies `recommended` for Orleans/Durable Task becomes first-party; every fault, dist, conc and msg row is re-checked for checks Orleans/DTF meet that the snapshot did not credit, scored by the third-party rule (PLAN-OF-PLANS §7.0 step 1); results with per-cell evidence in docs/scorecard/alignment-2.2.json; METHOD §8 logs it as a scope change (snapshot date unchanged); the leaf evidence states .NET U/E/RT before and after.'
  - 'Step 3 (bump to 2.2): method_version "2.2"; every stored version 0.6.0–0.10.0 re-scored; docs/scorecard/results/*.json, REPORT-method-v2.md and index.html regenerated; METHOD §5.2 restated with the 2.2 table and the 2.1 table kept as history; new fixtures fixtures/non-goal-row.json and fixtures/non-goal-check.json with expected outputs, run by --self-test.'
  - 'Targets: web.rpc and web.realtime 0.11.0 targets move to 0.12.0 (L15); bonus 0.11.0 targets conc.stm 89, sec.supply 100, test.qa 100 (0.11.0.md D1); web.graphql targets removed.'
  - 'PLAN-OF-PLANS: §1 north-star rows and every "re-based" §6 cell restated from `scripts/scorecard.sh --targets <v>` under 2.2; the §6 note "E 73.3 at 0.11" replaced; the rule "a met check is never a non-goal" recorded (dx.repl#3 stays scored, D3); the 0.11.0.md frontmatter `scorecard:` block and its "Capacity and index effect" table replaced by tool output.'
  - 'Step 4: the owner acceptance of the re-based numbers is recorded in PLAN-OF-PLANS §11 with a date. Until it is recorded the leaf is done-pending-acceptance and 0.11.0.md keeps `provisional: true`; this leaf never records acceptance on the owner''s behalf.'
  - 'Gate: `bash scripts/scorecard.sh --validate --self-test`; `bash scripts/scorecard.sh --method-compare && git diff --exit-code docs/scorecard/REPORT-method-v2.md`; `bash scripts/scorecard.sh --version 0.10.0`; `bash scripts/scorecard.sh --targets 0.11.0` prints the numbers written into 0.11.0.md'
  - 'Owns: `docs/scorecard/{METHOD.md,scorecard.json,scorecard.schema.json,alignment-2.2.json,fixtures/**,results/*.json,REPORT-method-v2.md,index.html}`, `scripts/scorecard.sh`, `.maestro/specs/roadmap/PLAN-OF-PLANS.md` (§1, §6, §11), `.maestro/specs/releases/0.11.0.md` (frontmatter `scorecard:` block and the "Capacity and index effect" table only)'
---

# r11-m22-rebase

Leaf of `.maestro/specs/releases/0.11.0.md`, sections "Step 0 — method 2.2" and "§7.0b and T0".
The release spec's row is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1, **first leaf** (PLAN-OF-PLANS §7.0b: "The first 0.11 leaf runs before scope selection").
Blocked by: the PLAN-OF-PLANS L15 update (branch chore/semver-baseline-0.10) merged into the release
branch.

Planning estimate to check against: `.tmp/plan11/m22.py` models step 1 only (0.10.0 measured:
id_effect E 75.5 → 76.6, .NET 77.3 → 77.9; committed 0.11 plan: 78.0 → 79.1). It reproduces the 2.1
numbers exactly (75.5, 77.3, 78.8), so differences after this leaf come from step 2 or from
implementation choices in `scripts/scorecard.sh`, and the leaf evidence explains them.
