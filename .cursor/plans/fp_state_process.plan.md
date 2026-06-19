---
name: FP State Machines
overview: id_effect_fsm: typed FSM, saga, session types. Part V ch19.
maestro:
  mission_id: pln-mqjykov9-bcblcg
  spec_path: .maestro/specs/fp-fsm.md
  execution_overlay: .maestro/missions/fp-fsm.execution.md
isProject: false
---

# FP State Machines

id_effect_fsm: typed FSM, saga, session types. Part V ch19.

See execution overlay: `.maestro/missions/fp-fsm.execution.md`

## Quality gates

| Gate | Command | Pass |
|------|---------|------|
| Format | `devenv shell -- moon run :ci-format` | exit 0 |
| Clippy | `devenv shell -- moon run :clippy` | exit 0 |
| Test | `devenv shell -- moon run :test` | exit 0 |
| Coverage | `devenv shell -- moon run :coverage` | ≥95% |
| Book | `devenv shell -- moon run :book` | exit 0 |
| Maestro verify | `maestro task verify <tsk-id>` | exit 0 |

## Maestro executor

Parallel waves per execution overlay. One subagent per leaf in parallel waves.
