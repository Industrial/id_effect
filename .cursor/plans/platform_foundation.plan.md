---
name: Platform Foundation
overview: "Complete id_effect_platform Phase A. Part VI ch26."
maestro:
  mission_id: pln-mqk3uzlz-ffz1p1
  spec_path: .maestro/specs/platform-foundation.md
  execution_overlay: .maestro/missions/platform-foundation.execution.md
isProject: false
---

# Platform Foundation

HTTP streaming, FS gaps, process cancellation, reqwest migration.

See execution overlay: `.maestro/missions/platform-foundation.execution.md`

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

