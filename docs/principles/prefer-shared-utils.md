# prefer-shared-utils

## Rule

When two or more features need the same primitive, use the helper in `src/shared/lib/` instead of duplicating per feature.

## Rationale

Duplicating shared primitives drifts implementations apart and hides bugs. Centralizing helpers in `src/shared/lib/` is what lets boundary checks enforce the layer rule.

## Scan Command

! rg -n "^export (function|const) (generateId|appendJsonl|toIsoDate|kebabize)\b" --glob 'src/features/**' --glob '!src/shared/**'

## Fix Recipe

1. Move the helper into `src/shared/lib/<helper>.ts`.
2. Replace each feature-local copy with `import { ... } from "@/shared/lib"`.
3. Delete the feature-local definition.
