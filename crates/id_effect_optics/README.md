# `id_effect_optics`

Functional optics for **immutable focus and update** in id_effect programs:

- **`Lens`** — total field access (`get`, `set`, `modify`, `compose`)
- **`Prism`** — partial variant access (`preview`, `review`, `modify`)
- **`Optional`** — helpers for `Option<T>` fields
- **`Traversal`** — map over every element in `Vec` / `im::Vector`
- **`Transducer`** — composable reducer transforms without intermediate collections
- **`schema_bridge`** — dot-path read/write on [`id_effect::schema::Unknown`]
- **`json_patch`** — RFC 6902 subset (`add`, `replace`, `remove`)
- **`TrieZipper`** — navigable trie stub (Part V ch18)

See the mdBook chapter [Optics with id_effect](../id_effect/book/src/part5/ch18-00-optics.md).

## Example

```bash
cargo run -p id_effect_optics --example 010_lens
```

## Part V

This crate implements **Plan 02 (FP Optics)** from `docs/fp-patterns/ROADMAP.md`.
