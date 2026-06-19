---
name: Platform DX and Ship
overview: "CLI parity, generators, deploy templates. Part VI ch34."
maestro:
  mission_id: pln-mqk3uymr-cb8ftt
  spec_path: .maestro/specs/platform-dx-ship.md
  execution_overlay: .maestro/missions/platform-dx-ship.execution.md
isProject: false
---

# Platform DX and Ship

Developer experience and deployment.

See execution overlay: `.maestro/missions/platform-dx-ship.execution.md`

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

