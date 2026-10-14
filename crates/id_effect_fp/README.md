# `id_effect_fp`

Functional-programming toolkit for [`id_effect`](../id_effect) — **maintenance tier**
([ADR 0010](../../docs/adrs/0010-crate-tiers.md)): kept building and tested, not on the core
roadmap.

These modules lived in the core `id_effect` crate until 0.6. The core runtime does not need them,
so they moved here together with their `im` and Rayon-backed bulk operations:

| Module | What | Formerly |
|--------|------|----------|
| `algebra` | `Semigroup`, `Monoid`, `Functor`, `Applicative`, `Monad`, `Bifunctor`, `Contravariant`, `Selective`, `Foldable`, `Traversable`, `FreeAp` | `id_effect::algebra` |
| `collections` | Persistent `EffectHashMap` / `EffectHashSet` / `EffectSortedMap` / `EffectSortedSet` / `EffectVector` / `RedBlackTree` (backed by `im`), `MutableList`, `MutableQueue`, `ChunkBuilder`, `Trie` | `id_effect::collections` |
| `stm` | `TRef`, `TMap`, `TQueue`, `TSemaphore`, `Stm`, `commit` / `atomically` | `id_effect::stm` |
| `foundation` | `Unit`, `Never`, `Iso`, products, coproducts, `function`, `option_` | `id_effect::foundation::*` |
| `result`, `reader`, `thunk` | Pure `Result` combinators, the `Reader` monad, one-shot thunks | `id_effect::kernel::{result, reader, thunk}` |

`Either`, `Pipe`, `Predicate`, `MutableRef` and `func` stay in `id_effect::foundation`.

## Example

```rust
use id_effect::run_blocking;
use id_effect_fp::{Monoid, Semigroup, TRef, commit, hash_map};

// STM: transactional reference cells, committed as an `Effect`.
let counter = run_blocking(commit(TRef::make(1_i32)), ()).expect("make");
run_blocking(commit(counter.update_stm::<(), _>(|n| n + 1)), ()).expect("update");
assert_eq!(run_blocking(commit(counter.read_stm::<()>()), ()), Ok(2));

// Persistent collections: updates return new maps; the old one is untouched.
let a = hash_map::set(&hash_map::empty(), "k", 1);
let b = hash_map::set(&a, "k", 2);
assert_eq!((hash_map::get(&a, "k"), hash_map::get(&b, "k")), (Some(&1), Some(&2)));

// Algebra: combine values through type-class traits.
assert_eq!(String::from("ab").combine(String::from("cd")), "abcd");
assert_eq!(<String as Monoid>::empty(), "");
```

## Examples

```bash
cargo run -p id_effect_fp --example 077_stm_tref
cargo run -p id_effect_fp --example 090_algebra_foldable
```

STM examples: `077`–`082`; algebra examples: `090`–`092`.
