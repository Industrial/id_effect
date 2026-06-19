# Optics with id_effect

**Part V · Chapter 18** — functional optics for immutable focus and update.

The [`id_effect_optics`](../../../../id_effect_optics/) crate provides:

- **Lens** — total field access (`get`, `set`, `modify`, `compose`, `as_traversal`)
- **Prism** — partial variant access (`preview`, `review`)
- **Optional** — helpers for `Option<T>` fields
- **Traversal** — map over vectors, optional fields, and composed optics
- **Iso** — bidirectional isomorphisms
- **Transducer** — composable reducer transforms
- **Schema bridge** — dot-path and JSON Pointer access on [`Unknown`](../../src/schema/parse.rs)
- **JSON Patch** — RFC 6902 operations on `Unknown`
- **TrieZipper** — navigable persistent trie with rebuild
- **`#[derive(Optics)]`** — codegen via `id_effect_proc_macro`

## When to reach for optics

Use optics when you need **composable, reusable focus** into nested data — especially persistent
[`im`](../../src/collections/mod.rs) structures or [`Unknown`](../../src/schema/parse.rs) documents at boundaries.

## Example

```bash
cargo run -p id_effect_optics --example 010_lens
cargo run -p id_effect_optics --example 030_schema_patch
```

## Sections

- [Lenses](ch18-01-lenses.md)
- [Prisms and Optionals](ch18-02-prisms-optionals.md)
- [Traversals and schema bridge](ch18-03-traversals-schema.md)
