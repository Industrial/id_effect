# Execution overlay: roadmap-bootstrap

Spec: `.maestro/specs/roadmap-bootstrap.md` · Plan: `.maestro/missions/roadmap-bootstrap.plan.md`
· Parent: `.maestro/specs/roadmap/PLAN-OF-PLANS.md`

| Wave | Tasks (slug) | Parallel? | Blocked by |
|------|--------------|-----------|------------|
| 0 | leaf-adr-0012, leaf-scorecard-tooling, leaf-close-w5 | yes (3 subagents) | — |
| 1 | leaf-scorecard-report, leaf-method-v2-checklists | yes (2 subagents; v2 may split into 4 by group) | wave 0 (leaf-scorecard-tooling) |

Rules: claim with `--skip-worktree`; never claim wave 1 before `leaf-scorecard-tooling` is shipped;
each leaf records gate evidence via `maestro_evidence_record` before `maestro_verdict_request`.
