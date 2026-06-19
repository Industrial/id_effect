---
name: Platform API Boundaries
overview: "RPC codegen, OpenAPI, API versioning. Part VI ch29."
maestro:
  mission_id: pln-mqk3uv7g-4r9v0a
  spec_path: .maestro/specs/platform-api-boundaries.md
  execution_overlay: .maestro/missions/platform-api-boundaries.execution.md
isProject: false
---

# Platform API Boundaries

Complete RPC and API surface.

See execution overlay: `.maestro/missions/platform-api-boundaries.execution.md`

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

