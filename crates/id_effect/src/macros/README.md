# `macros` — the macro surface

`id_effect::macros` re-exports the macros of the two sibling crates (Rust cannot mix
`macro_rules!` and `#[proc_macro]` in one crate). The same items are re-exported at the crate
root, so `use id_effect::{effect, caps};` is the usual import.

| Macro | Crate | Role |
|-------|-------|------|
| `effect!({ .. })` | `id_effect_proc_macro` | Do-notation over an async body building an `Effect`: `~Cap` reads a capability from `R`, `~expr` binds a sub-effect (widening its error), `~(a, b)` joins effects in parallel, `?` fails with a `Result`'s error. |
| `effect_local!({ .. })` | `id_effect_proc_macro` | The same, building a `LocalEffect` (`!Send` body, e.g. `Rc` across awaits). |
| `caps!(A, B)` | `id_effect_proc_macro` | Spells the typed environment `HCons<A, HCons<B, HNil>>`. |
| `pipe!(x, f, g)` | `id_effect_macro` | Left-to-right application. |
| `err!(A \| B \| C)` | `id_effect_macro` | The error union `Or<A, Or<B, C>>`. |

Also at the crate root (not in this module): `match_effect!`, `#[effect_tagged]`, and the
derives `EffectData`, `Schema`, `Optics` and `ProviderSpecDerive` (emits an inherent
`layer()` returning a `Layer`).

```rust
use id_effect::{Effect, Exit, Never, caps, effect};

#[derive(Debug)]
struct Counter(i32);

fn program() -> Effect<i32, Never, caps!(Counter)> {
  effect!({ ~Counter.0 + 1 })
}

assert_eq!(program().provide(Counter(41)).block_on(), Exit::succeed(42));
```

The 0.6 capability macros (`provide!`, `providers!`, `require!`, `mock_capability!` and the
`CapList` form of `caps!`) were removed in 0.7: build a `Layer`, `merge` layers, read with `~K`
or `need::<K>()`, and provide mocks as plain values (`.provide(mock)`).

## See also

- [`crates/id_effect_proc_macro`](../../../id_effect_proc_macro) and
  [`crates/id_effect_macro`](../../../id_effect_macro) — the implementations.
- `crates/id_effect/tests/macros.rs` and `tests/ui` — expansion and diagnostic tests.
