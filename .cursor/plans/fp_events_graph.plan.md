---
name: FP Events and Graph
overview: EventStore, projections, id_effect_graph. Part V ch23.
maestro:
  mission_id: pln-mqjykpbf-qbf6qg
  spec_path: .maestro/specs/fp-events.md
  execution_overlay: .maestro/missions/fp-events.execution.md
isProject: false
---

# FP Events and Graph

EventStore, projections, id_effect_graph. Part V ch23.

See execution overlay: `.maestro/missions/fp-events.execution.md`

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
