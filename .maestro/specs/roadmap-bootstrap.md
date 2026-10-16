---
title: Roadmap bootstrap — platform vision ADR, reproducible scorecard, release protocol
slug: roadmap-bootstrap
mode: heavy
work_type: initiative
risk_class: high
version: 1
acceptance_criteria:
  - docs/adrs/0012-platform-vision.md (status Accepted) supersedes the ADR 0010 vision statement, adds the "platform" tier (integration gates + reference-app consumer), and amends Rule 4 so CI-tested reference apps count as consumers; ADR 0010 gets a "Superseded in part by 0012" header line; README scope paragraph (lines 3-5) no longer says "not a batteries-included web framework".
  - docs/scorecard/ contains METHOD.md, scorecard.json (moved from .maestro/specs/roadmap/scorecard/), results/v0.6.0.json and results/v0.7.0.json; scripts/scorecard.sh (bash + jq, Requires header, exit 1 FAIL / 2 misuse) implements METHOD §5 and §5.1; `scripts/scorecard.sh --self-test` reproduces every number in METHOD §5.2 exactly (one-decimal string compare).
  - scorecard.sh subcommands work - `--version <v>` (indices + KPIs), `--targets <v>` (planned indices), `--diff <a> <b>` (row deltas, exit 1 on any row ≤ −5 without `adr` note), `--compare-targets <v>` (exit 1 if E < target − 1.0), `--report <v>` (writes REPORT-<v>.md), `--html` (writes docs/scorecard/index.html from the JSON).
  - scorecard.json validates against docs/scorecard/scorecard.schema.json (row ids unique, 10 competitor scores, scores in 1..100, every group/track referenced exists); invalid fixtures under docs/scorecard/fixtures/ make the validator exit 1.
  - moon task `scorecard` and a CI step in ci.yml run `scorecard.sh --self-test` and schema validation on every PR.
  - .maestro/specs/roadmap/PLAN-OF-PLANS.md links to docs/scorecard/ paths (no stale .maestro scorecard paths remain - grep returns 0).
  - Stale task tsk-mv799sew-y9wf9g (W5, migrate downstream to 0.6) is abandoned with reason "superseded by PLAN-OF-PLANS E0.5 (downstream migration to 0.7+)"; mission pln-mv799j4f-1r1u6b is closed.
  - Method v2 table-stakes checklists exist for all 82 rows in scorecard.json (a checks list with 4-8 entries each); scorecard.sh --method 2 recomputes 0.6.0 and 0.7.0; REPORT-method-v2.md shows v1 vs v2 side by side for both versions.
non_goals:
  - Any 0.8 product work (license change, CLI, batteries) - that is release-0-8, scoped at its own kickoff.
  - Re-scoring competitor columns (snapshot frozen until 1.0, METHOD §4).
  - Running performance benches in CI.
---

# Roadmap bootstrap

Turns the plan of plans (`.maestro/specs/roadmap/PLAN-OF-PLANS.md`) into running machinery
before 0.8 kickoff: the vision ADR that permits it, the scorecard as a reproducible tool, and the
v2 checklists that make re-scoring mechanical. Execution plan:
`.maestro/missions/roadmap-bootstrap.plan.md`; waves: `.maestro/missions/roadmap-bootstrap.execution.md`.
