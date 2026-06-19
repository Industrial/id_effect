---
name: Platform Workflow Cluster
overview: "Durable workflow beyond SQLite spike. Part VI ch32."
maestro:
  mission_id: pln-mqk3v34c-c8b55j
  spec_path: .maestro/specs/platform-workflow-cluster.md
  execution_overlay: .maestro/missions/platform-workflow-cluster.execution.md
isProject: false
---

# Platform Workflow Cluster

Phase G cluster and workflow extension.

See execution overlay: `.maestro/missions/platform-workflow-cluster.execution.md`

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

