# `foundation` — small pure building blocks

Kernel-independent helpers with no I/O and no `Effect`:

| Module | Items |
|--------|-------|
| `either` | `Either` and Effect.ts-style helpers on it. |
| `func` | `identity`, `always`, `compose`, `absurd`. |
| `piping` | The `Pipe` trait (`x.pipe(f)`); the `pipe!` macro lives in `id_effect_macro`. |
| `predicate` | `Predicate<A>`: composable `A -> bool` (`and`, `or`, `not`). |

Since 0.6, `unit`, `never`, `function`, `product`, `coproduct`, `isomorphism` and `option_` live
in `id_effect_fp::foundation` and `algebra` in `id_effect_fp::algebra`; 0.7 removed
`mutable_ref` (use `Ref` or a `FiberRef` inside effects) and trimmed `func` to the four items
above.
