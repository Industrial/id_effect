---
name: Platform Kitchen Sink Meta
overview: "Orchestration: specs, plans, missions, unified roadmap. No product code."
maestro:
  mission_id: pln-mqk3v0l4-66d3zl
  spec_path: .maestro/specs/platform-kitchen-sink-meta.md
  execution_overlay: .maestro/missions/platform-kitchen-sink-meta.execution.md
isProject: false
---

# Platform Kitchen Sink Meta

Orchestration: specs, plans, missions, unified roadmap, book bootstrap. No product code.

See execution overlay: `.maestro/missions/platform-kitchen-sink-meta.execution.md`

## Quality gates

| Gate | Command | Pass |
|------|---------|------|
| Format | `devenv shell -- moon run :ci-format` | exit 0 |
| Clippy | `devenv shell -- moon run :clippy` | exit 0 |
| Test | `devenv shell -- moon run :test` | exit 0 |
| Coverage | `devenv shell -- moon run effect:coverage` | ≥95% |
| Book | `devenv shell -- moon run :book` | exit 0 |
| Maestro verify | `maestro task verify <tsk-id>` | exit 0 |

## Maestro executor

Parallel waves per execution overlay. One subagent per leaf in parallel waves.

