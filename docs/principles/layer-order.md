# layer-order

## Rule

Layers import forward-only along `types -> config -> repo -> service -> runtime -> ui`. Providers are cross-cutting and universally importable. No layer may import a sibling further down the stack.

## Rationale

Layer-order is the spine of the architecture (ADR-0017). Mechanical enforcement keeps boundaries real instead of aspirational.

## Scan Command

bun run lint:arch

## Fix Recipe

1. Identify the offending import in the lint output.
2. Move the dependency up the stack rather than reaching down.
3. Re-run `bun run lint:arch` until the violation set is empty.
