# `schema` — Stratum 13: data, equality, ordering & parsing

**Type-safe data descriptions** and validation: [`Brand`](brand.rs) / [`RefinedBrand`](brand.rs), [`Equal`](equal.rs) / [`EffectHash`](equal.rs), [`EffectData`](data.rs) ([`DataStruct`](data.rs), [`DataTuple`](data.rs), [`DataError`](data.rs)), [`Ordering`](order.rs) / [`DynOrder`](order.rs), and [`Schema`](parse.rs) with [`ParseError`](parse.rs) / [`Unknown`](parse.rs). Combinators like `struct_`, `tuple`, `optional`, `refine` live in `parse.rs`.

## What lives here

| Module | Role |
|--------|------|
| `brand` | Newtypes / refined brands for domain tagging. |
| `equal` | Structural equality and hashing hooks used by data classes. |
| `data` | `EffectData` trait — generic data metadata (constructor arity, field access). |
| `order` | Total/preorder helpers, dynamic orderings. |
| `parse` | `Schema` — decode/encode-style validation pipelines from unknown input. |

## What it is used for

- **Config and wire formats** — parse untrusted input into strong types (`Schema`, `ParseError`).
- **Stable hashing** for caches and collections (`EffectHash`, `Equal`).
- **Derive-like** patterns via `EffectData` and proc-macro support (`EffectData` in `effect-proc-macro`).

## Best practices

1. **Fail closed** — treat `Unknown` as hostile until `Schema` succeeds; log `ParseError` with context, not raw secrets.
2. **Keep `Equal`/`EffectHash` aligned** with Rust `Eq`/`Hash` laws when mirroring std collections.
3. **Prefer small composable schemas** (`tuple`, `struct_`) over one giant validator function.
4. **See** in-tree [`SPEC.md`](SPEC.md) for schema-specific notation and edge cases.

## See also

- [`schema/SPEC.md`](SPEC.md) — deeper spec for this subtree.
- [`SPEC.md`](../../SPEC.md) §Stratum 13.
- [`collections`](../collections/README.md) — structures that may hold branded values.
