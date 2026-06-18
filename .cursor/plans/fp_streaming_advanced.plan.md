---
name: FP Advanced Streaming
overview: Stream windowing, joins, transducers. Part V ch22.
maestro:
  mission_id: pln-mqjykp6a-uo9tbq
  spec_path: .maestro/specs/fp-streaming.md
  execution_overlay: .maestro/missions/fp-streaming.execution.md
isProject: false
---

# FP Advanced Streaming

Stream windowing, joins, transducers. Part V ch22.

See execution overlay: `.maestro/missions/fp-streaming.execution.md`

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
