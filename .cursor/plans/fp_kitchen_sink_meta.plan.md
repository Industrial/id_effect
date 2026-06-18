---
name: FP Kitchen Sink Meta
overview: Orchestration: specs, plans, missions, roadmap. No product code.
maestro:
  mission_id: pln-mqjykoig-a58dfk
  spec_path: .maestro/specs/fp-kitchen-sink-meta.md
  execution_overlay: .maestro/missions/fp-kitchen-sink-meta.execution.md
isProject: false
---

# FP Kitchen Sink Meta

Orchestration: specs, plans, missions, roadmap. No product code.

See execution overlay: `.maestro/missions/fp-kitchen-sink-meta.execution.md`

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
