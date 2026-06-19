---
name: Platform AI
overview: "id_effect_ai LLM traits, MCP template. Part VI ch35."
maestro:
  mission_id: pln-mqk3uu83-9z30eo
  spec_path: .maestro/specs/platform-ai.md
  execution_overlay: .maestro/missions/platform-ai.execution.md
isProject: false
---

# Platform AI

AI client abstractions.

See execution overlay: `.maestro/missions/platform-ai.execution.md`

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

