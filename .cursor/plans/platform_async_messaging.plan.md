---
name: Platform Async Messaging
overview: "Jobs, outbox, broker adapters, events journal. Part VI ch31."
maestro:
  mission_id: pln-mqk3ux00-wrxxug
  spec_path: .maestro/specs/platform-async-messaging.md
  execution_overlay: .maestro/missions/platform-async-messaging.execution.md
isProject: false
---

# Platform Async Messaging

Background jobs and messaging.

See execution overlay: `.maestro/missions/platform-async-messaging.execution.md`

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

