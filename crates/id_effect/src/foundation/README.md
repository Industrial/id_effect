# `foundation` — Stratum 0: categorical bedrock

This directory implements the **lowest stratum** of the `effect` crate: small, law-abiding building blocks (products, coproducts, morphisms) that every higher layer assumes. Nothing here performs I/O or knows about `Effect`; it is pure structure and utilities.

> Since 0.6 only `either`, `func`, `piping` and `predicate` remain here; `unit`,
> `never`, `function`, `product`, `coproduct`, `isomorphism` and `option_` moved to
> `id_effect_fp::foundation`, and `algebra` to `id_effect_fp::algebra`. 0.7 removed
> `mutable_ref` and trimmed `func` to `identity`, `always`, `compose`, `absurd`.

## What lives here

| Module | Construct | Role |
|--------|-----------|------|
| `unit` | `Unit` | Terminal object; trivial success / “no information”. |
| `never` | `Never` | Initial object; uninhabited type for impossible branches (`absurd`). |
| `function` | `identity`, `compose`, `const_`, `pipe1`/`pipe2`/`pipe3`, `flip`, `always` | Morphisms and plumbing. |
| `product` | `pair`, `fst`, `snd`, `swap`, `bimap_product` | Categorical product (pairs). |
| `coproduct` | `Either`, `left`, `right`, `either` | Coproduct / sum types (aligned with `Result`/`Either` conventions). |
| `isomorphism` | `Iso<A, B>` | Witnesses structure-preserving back-and-forth maps. |
| `either` | Effect.ts–style helpers on `Either` | Ergonomic combinators over the coproduct. |
| `func` | `identity`, `always`, `compose`, `absurd` | Function plumbing core relies on. |
| `option_` | Free functions over `Option<T>` | Shared `Option` vocabulary. |
| `piping` | `Pipe` trait | Method-style piping: `x.pipe(f)` (mirrors fluent style). |
| `predicate` | `Predicate<A>` | Composable `A -> bool` wrappers. |

## What it is used for

- **Composing** higher strata without duplicating trivial helpers.
- **Stating laws** (identity, associativity) at the leaves; tests target 100% branch coverage on tiny functions (see repo `TESTING.md`).
- **Aligning** naming with Effect.ts / FP idioms (`Either`, piping) so application code reads consistently.

## Best practices

1. **Prefer these primitives** over ad hoc tuples and closures when the same pattern appears twice—keeps laws and tests centralized.
2. **Do not import `foundation` for I/O**; environment and effects live in `kernel`, `context`, and `runtime`.
3. **No hidden global mutable cells**—inside `Effect` graphs, prefer `FiberRef`, `TRef`, or explicit `R` services.
4. **Preserve isomorphism round-trips** when extending `Iso`; add tests `iso.to(iso.from(x)) ≈ x` for new witnesses.
5. **Use `Predicate`** when you need combinators (`and`, `or`, `not`) over plain `fn(&A) -> bool` to avoid boolean soup.

## See also

- Crate-level [`SPEC.md`](../../SPEC.md) §Stratum 0.
- [`algebra`](../algebra/README.md) — type-class–style traits built on these foundations.
