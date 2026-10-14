# `collections` — Stratum 14: persistent & mutable collections

**Persistent** data structures (via the [`im`](../../lib.rs) re-export) and **mutex-guarded** mutable collections for shared effect state. Types are named to mirror **Effect.ts** (`EffectHashMap`, …).

## What lives here

| Module | Type / role |
|--------|-------------|
| `hash_map` | `EffectHashMap`, `MutableHashMap` — HAMT maps. |
| `hash_set` | `EffectHashSet`, `MutableHashSet` — HAMT sets. |
| `sorted_map` | `EffectSortedMap` — ordered map (`im::OrdMap`). |
| `sorted_set` | `EffectSortedSet` — ordered set (`im::OrdSet`). |
| `vector` | `EffectVector` — `im::Vector` (RRB). |
| `red_black_tree` | `RedBlackTree` — ordered multimap view. |
| `mutable_list` | `MutableList`, `ChunkBuilder` — shared deque-style list. |
| `mutable_queue` | `MutableQueue` — bounded/unbounded FIFO. |
| `trie` | `Trie` — string-keyed trie. |

## What it is used for

- **Pure functional updates** in algorithms (persistent `im` types — cheap structural sharing).
- **Shared** append-only or bounded queues between fibers (`MutableQueue`, `MutableList`).
- **Cross-crate compatibility** — depend on `effect::im` for the same `im` version as these aliases.

## Best practices

1. **Pick persistent vs mutable** deliberately — persistent for forked values; mutex-backed for shared hot buffers.
2. **Import `effect::im`** from this crate to avoid version skew with `EffectHashMap` et al.
3. **Profile** large `OrdMap`/`Vector` paths — log-n is predictable but not free.
4. **Do not** nest heavy locks across collections without a deadlock plan (see [`coordination`](../coordination/README.md)).

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 14.
- [`schema`](../schema/README.md) — hashing/equality ties into map keys.
- Crate root [`lib.rs`](../lib.rs) — `pub use im`.
