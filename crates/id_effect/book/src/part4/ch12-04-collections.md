# TQueue, TMap, TSemaphore — Transactional Collections

`id_effect_fp` builds three collections on top of `TRef`. Their operations return `Stm` values, so
they compose with `TRef` operations in the same transaction. Each constructor is a transaction
too: commit it once and share the handle (all three are `Clone + Send + Sync`).

| Type | Constructor | Operations |
|------|-------------|------------|
| `TQueue<A>` | `TQueue::bounded(n)`, `TQueue::unbounded()` | `offer(a) -> Stm<bool, E>` (`false` when full), `take() -> Stm<A, E>` (retries while empty) |
| `TMap<K, V>` | `TMap::make()` | `get(&k) -> Stm<Option<V>, E>`, `set(k, v)`, `delete(&k)` |
| `TSemaphore` | `TSemaphore::make(n)` | `acquire()` (retries while no permit), `release()` |

## TQueue: producer / consumer

`take` waits (retries) while the queue is empty. `offer` on a full bounded queue does *not* wait:
it returns `false`. Add `Stm::check` to make a waiting offer:

```rust
use id_effect_fp::{Stm, TQueue, commit};
# use id_effect::{Cause, Effect, Exit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }

fn offer_blocking(queue: &TQueue<u32>, job: u32) -> Stm<(), ()> {
  queue.offer(job).flat_map(Stm::<(), ()>::check) // retry while full
}

let queue = run(commit(TQueue::<u32>::bounded(2))).expect("queue");
let producer = {
  let queue = queue.clone();
  std::thread::spawn(move || {
    for job in 1..=10 {
      run(commit(offer_blocking(&queue, job))).expect("offer");
    }
  })
};
let sum: u32 = (0..10)
  .map(|_| run(commit(queue.take::<()>())).expect("take")) // waits while empty
  .sum();
producer.join().expect("join");
assert_eq!(sum, 55);
```

## TMap: atomic with other cells

Reads and writes on a `TMap` and a `TRef` in one transaction commit together:

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect_fp::{TMap, TRef, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let users = run(commit(TMap::<String, String>::make())).expect("map");
let reads = run(commit(TRef::make(0_u32))).expect("cell");

let lookup = {
  let reads = reads.clone();
  users
    .get::<()>(&"alice".to_string())
    .flat_map(move |user| reads.update_stm(|n| n + 1).map(move |()| user.clone()))
};
// Either the map read AND the counter increment happen, or neither does.
let user: Option<String> = run(commit(lookup))?;
# assert_eq!(user, None);
# assert_eq!(run(commit(reads.read_stm::<()>())), Ok(1));
# Ok::<(), ()>(())
```

## TSemaphore: transactional permits

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect_fp::{Stm, TSemaphore, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let sem = run(commit(TSemaphore::make(1))).expect("sem");
run(commit(sem.acquire::<()>()))?; // takes the only permit

// Non-blocking try: `or_else` runs when `acquire` would retry.
let try_acquire = sem.acquire::<()>().map(|()| true).or_else(Stm::succeed(false));
assert_eq!(run(commit(try_acquire.clone())), Ok(false));

run(commit(sem.release::<()>()))?;
assert_eq!(run(commit(try_acquire)), Ok(true));
# Ok::<(), ()>(())
```

Because `acquire` and `release` are `Stm`s, you can combine taking a permit with other state
changes in one transaction. To bound concurrent *effects* (permits held across I/O), use core
`id_effect::Semaphore` instead; STM transactions must not span I/O.

Full example: [`crates/id_effect_fp/examples/086_stm_collections.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/examples/086_stm_collections.rs) (`cargo run -p id_effect_fp --example 086_stm_collections`); minimal per-type examples are `080_stm_tmap`, `081_stm_tqueue`, `082_stm_tsemaphore`.

## Summary

| Type | Purpose |
|------|---------|
| `TRef<T>` | Single mutable value |
| `TQueue<T>` | FIFO queue; `take` waits while empty |
| `TMap<K, V>` | Transactional hash map |
| `TSemaphore` | Transactional permit counter |

All compose with `flat_map` and commit atomically with other STM operations.
