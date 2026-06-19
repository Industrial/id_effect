---
name: Platform UI Realtime
overview: "Dioxus SSR bridge, realtime channels. Part VII ch33."
maestro:
  mission_id: pln-mqk3v2du-i1jila
  spec_path: .maestro/specs/platform-ui-realtime.md
  execution_overlay: .maestro/missions/platform-ui-realtime.execution.md
isProject: false
---

# Platform UI Realtime

Full-stack UI with Dioxus first.

See execution overlay: `.maestro/missions/platform-ui-realtime.execution.md`

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

