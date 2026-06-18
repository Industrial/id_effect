---
name: FP Parser Combinators
overview: id_effect_parse: combinators, Pretty, Diff. Part V ch20.
maestro:
  mission_id: pln-mqjykox8-rmjgqz
  spec_path: .maestro/specs/fp-parse.md
  execution_overlay: .maestro/missions/fp-parse.execution.md
isProject: false
---

# FP Parser Combinators

id_effect_parse: combinators, Pretty, Diff. Part V ch20.

See execution overlay: `.maestro/missions/fp-parse.execution.md`

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
