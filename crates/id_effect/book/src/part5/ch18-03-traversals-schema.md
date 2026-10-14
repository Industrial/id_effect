# Traversals and schema bridge

## Traversals

[`Traversal`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/traversal.rs) maps over **zero or many** inner values:

```rust
use id_effect_optics::{vector_each, at_vec, at_option, field};

let doubled = vector_each::<i32>().over(vec![1, 2, 3], |n| n * 2);
```

Compose a field lens with collection traversals via [`at_vec`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/traversal.rs) and [`at_option`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/traversal.rs).

## Schema field paths

[`get_at_path`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/schema_bridge.rs), [`set_at_path`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/schema_bridge.rs), and [`create_at_path`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/schema_bridge.rs) navigate [`Unknown`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/schema/parse.rs) with dot-separated segments (`user.name`, `tags.0`) or JSON Pointer paths (`/user/name/0`).

## JSON Patch

[`apply_patch`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/json_patch.rs) supports `add`, `replace`, `remove`, `move`, `copy`, and `test`.

## Trie zipper

[`TrieZipper`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/zipper.rs) navigates persistent tries, supports `rebuild`, `insert_child`, and `remove_child`.
