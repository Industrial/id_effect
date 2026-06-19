---
name: Platform Observability
overview: "OTEL workspace member, starter layer, health checks. Part VI ch27."
maestro:
  mission_id: pln-mqk3v1k9-6c8qcj
  spec_path: .maestro/specs/platform-observability.md
  execution_overlay: .maestro/missions/platform-observability.execution.md
isProject: false
---

# Platform Observability

OpenTelemetry integration and health endpoints.

See execution overlay: `.maestro/missions/platform-observability.execution.md`

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

