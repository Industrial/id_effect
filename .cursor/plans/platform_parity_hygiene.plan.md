---
name: Platform Parity Hygiene
overview: "Upstream Effect.ts checklist automation and parity docs."
maestro:
  mission_id: pln-mqk3tuel-6cy7au
  spec_path: .maestro/specs/platform-parity-hygiene.md
  execution_overlay: .maestro/missions/platform-parity-hygiene.execution.md
isProject: false
---

# Platform Parity Hygiene

Phase I ongoing maintenance.

See execution overlay: `.maestro/missions/platform-parity-hygiene.execution.md`

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

