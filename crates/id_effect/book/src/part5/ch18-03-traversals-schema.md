# Traversals and schema bridge

## Traversals

[`Traversal`](../../../../id_effect_optics/src/traversal.rs) maps over **zero or many** inner values:

```rust
use id_effect_optics::vector_each;

let doubled = vector_each::<i32>().over(vec![1, 2, 3], |n| n * 2);
```

## Schema field paths

[`get_at_path`](../../../../id_effect_optics/src/schema_bridge.rs) / [`set_at_path`](../../../../id_effect_optics/src/schema_bridge.rs) navigate [`Unknown`](../../src/schema/parse.rs) with dot-separated segments (`user.name`, `tags.0`).

## JSON Patch subset

[`apply_patch`](../../../../id_effect_optics/src/json_patch.rs) supports `add`, `replace`, and `remove`.

## Trie zipper

[`TrieZipper`](../../../../id_effect_optics/src/zipper.rs) is a navigation stub over persistent tries — full zipper algebra is future work.

> **Stub:** transducers tie into ch22 streaming advanced.
