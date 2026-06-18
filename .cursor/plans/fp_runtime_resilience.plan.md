---
name: FP Runtime Resilience
overview: RequestResolver, SubscriptionRef, id_effect_resilience. Part V ch21.
maestro:
  mission_id: pln-mqjykp1v-cdoxhr
  spec_path: .maestro/specs/fp-runtime.md
  execution_overlay: .maestro/missions/fp-runtime.execution.md
isProject: false
---

# FP Runtime Resilience

RequestResolver, SubscriptionRef, id_effect_resilience. Part V ch21.

See execution overlay: `.maestro/missions/fp-runtime.execution.md`

## Quality gates

| Gate | Command | Pass |
|------|---------|------|
| Format | `devenv shell -- moon run :ci-format` | exit 0 |
| Clippy | `devenv shell -- moon run :clippy` | exit 0 |
| Test | `devenv shell -- moon run :test` | exit 0 |
| Coverage | `devenv shell -- moon run :coverage` | ≥95% |
| Book | `devenv shell -- moon run :book` | exit 0 |
| Maestro verify | `maestro task verify <tsk-id>` | exit 0 |

## Maestro executor

Parallel waves per execution overlay. One subagent per leaf in parallel waves.
