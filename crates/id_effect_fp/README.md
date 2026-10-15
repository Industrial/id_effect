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
use id_effect::Exit;
use id_effect_fp::{Monoid, Semigroup, TRef, commit, hash_map};

// STM: transactional reference cells, committed as a next-kernel `Effect`.
let counter = commit(TRef::make(1_i32)).block_on().into_result().expect("make");
commit(counter.update_stm::<(), _>(|n| n + 1)).block_on().into_result().expect("update");
assert_eq!(commit(counter.read_stm::<()>()).block_on(), Exit::succeed(2));

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

STM examples: `077`–`086`; algebra examples: `090`–`092`.

## STM retry

`commit` runs on the 0.7 kernel (`id_effect::next`). A transaction that retries
(`Stm::retry`, `Stm::check(false)`, `TQueue::take` on an empty queue, `TSemaphore::acquire` at
zero) parks its fiber on the wait lists of the `TRef`s it read, and the next commit that writes
one of them wakes it (Effect.ts `STM.retry`). Nothing spins and no thread is blocked; an
interrupted waiter is removed from every wait list.
