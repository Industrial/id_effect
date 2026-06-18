---
name: FP Optics
overview: id_effect_optics: Lens, Prism, Traversal, Schema bridge. Part V ch18.
maestro:
  mission_id: pln-mqjykos6-ovw199
  spec_path: .maestro/specs/fp-optics.md
  execution_overlay: .maestro/missions/fp-optics.execution.md
isProject: false
---

# FP Optics

id_effect_optics: Lens, Prism, Traversal, Schema bridge. Part V ch18.

See execution overlay: `.maestro/missions/fp-optics.execution.md`

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
