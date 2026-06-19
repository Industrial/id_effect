# `id_effect_optics`

Functional optics for **immutable focus and update** in id_effect programs:

- **`Lens`** — total field access (`get`, `set`, `modify`, `compose`, `as_traversal`)
- **`Prism`** — partial variant access (`preview`, `review`, `modify`)
- **`Optional`** — helpers for `Option<T>` fields (`compose` with inner lenses)
- **`Traversal`** — map over every element in vectors, optional fields, and composed optics
- **`Iso`** — bidirectional isomorphisms with `as_lens`
- **`Transducer`** — composable reducer transforms (`map`, `filter`, `take`)
- **`schema_bridge`** — dot-path and JSON Pointer read/write/create on [`id_effect::schema::Unknown`]
- **`json_patch`** — RFC 6902 operations (`add`, `replace`, `remove`, `move`, `copy`, `test`)
- **`TrieZipper`** — navigable persistent trie with rebuild/insert/remove
- **`#[derive(Optics)]`** — field lenses and enum prisms via `id_effect_proc_macro`

See the mdBook chapter [Optics with id_effect](../id_effect/book/src/part5/ch18-00-optics.md).

## Examples

```bash
cargo run -p id_effect_optics --example 010_lens
cargo run -p id_effect_optics --example 020_prism
cargo run -p id_effect_optics --example 030_schema_patch
cargo run -p id_effect_optics --example 040_traversal
cargo run -p id_effect_optics --example 050_zipper
```

## Part V

This crate implements **Plan 02 (FP Optics)** from `docs/fp-patterns/ROADMAP.md`.
