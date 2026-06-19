---
name: FP DX Metaprogramming
overview: Law tests, derives, FreeAp. Part V ch24.
maestro:
  mission_id: pln-mqjykpfk-a28tq5
  spec_path: .maestro/specs/fp-dx.md
  execution_overlay: .maestro/missions/fp-dx.execution.md
isProject: false
---

# FP DX Metaprogramming

Law tests, derives, FreeAp. Part V ch24.

See execution overlay: `.maestro/missions/fp-dx.execution.md`

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
