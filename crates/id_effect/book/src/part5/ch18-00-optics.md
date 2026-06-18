# Optics with id_effect

**Part V · Chapter 18** — functional optics for immutable focus and update.

The [`id_effect_optics`](../../../../id_effect_optics/) crate provides:

- **Lens** — total field access (`get`, `set`, `modify`, `compose`)
- **Prism** — partial variant access (`preview`, `review`)
- **Optional** — helpers for `Option<T>` fields
- **Traversal** — map over every element in `Vec` / `im::Vector`
- **Transducer** — composable reducer transforms
- **Schema bridge** — dot-path read/write on [`Unknown`](../../src/schema/parse.rs)
- **JSON Patch** — RFC 6902 subset (`add`, `replace`, `remove`)
- **TrieZipper** — navigable trie stub

## When to reach for optics

Use optics when you need **composable, reusable focus** into nested data — especially persistent
[`im`](../../src/collections/mod.rs) structures or [`Unknown`](../../src/schema/parse.rs) documents at boundaries.

## Example

```bash
cargo run -p id_effect_optics --example 010_lens
```

## Sections

- [Lenses](ch18-01-lenses.md)
- [Prisms and Optionals](ch18-02-prisms-optionals.md)
- [Traversals and schema bridge](ch18-03-traversals-schema.md)
