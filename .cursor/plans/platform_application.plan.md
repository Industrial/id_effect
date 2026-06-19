---
name: Platform Application Host
overview: "id_effect_host, auth, security middleware. Part VI ch30."
maestro:
  mission_id: pln-mqk3uw14-l2rehg
  spec_path: .maestro/specs/platform-application.md
  execution_overlay: .maestro/missions/platform-application.execution.md
isProject: false
---

# Platform Application Host

Application lifecycle and auth traits.

See execution overlay: `.maestro/missions/platform-application.execution.md`

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

