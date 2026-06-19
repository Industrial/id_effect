---
name: id_effect-algebra
description: >-
  Teaches id_effect algebra stratum: Foldable, Alternative, Traversable,
  Bifoldable, Invariant. Use when extending type classes or using traverse/alt/fold.
---

# id_effect Algebra

**Part V ch17**. Prerequisite: `id_effect-fundamentals`.

## Modules

| Module | Use |
|--------|-----|
| `algebra::foldable` | `fold_right` on Option, Vec, EffectVector |
| `algebra::alternative` | `empty`, `alt` for validation accumulation |
| `algebra::traversable` | `traverse_vec`, `sequence_vec`, `traverse_option` |
| `algebra::bifoldable` | `bifold`, `bitraverse` on Either |
| `algebra::invariant` | `imap` for newtypes |

## Verify

```bash
cargo test -p id_effect --lib algebra
```
