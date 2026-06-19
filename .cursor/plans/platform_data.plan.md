---
name: Platform Data
overview: "id_effect_sql + PostgreSQL driver. Part VI ch28."
maestro:
  mission_id: pln-mqk3uxtf-055tfk
  spec_path: .maestro/specs/platform-data.md
  execution_overlay: .maestro/missions/platform-data.execution.md
isProject: false
---

# Platform Data

Effect-native SQL client and transactions.

See execution overlay: `.maestro/missions/platform-data.execution.md`

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

