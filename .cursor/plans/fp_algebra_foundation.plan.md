---
name: FP Algebra Foundation
overview: Foldable, Alternative, Traversable, Bifoldable, Invariant. Part V ch17.
maestro:
  mission_id: pln-mqjykorq-bvmr7a
  spec_path: .maestro/specs/fp-algebra.md
  execution_overlay: .maestro/missions/fp-algebra.execution.md
isProject: false
---

# FP Algebra Foundation

Foldable, Alternative, Traversable, Bifoldable, Invariant. Part V ch17.

See execution overlay: `.maestro/missions/fp-algebra.execution.md`

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
