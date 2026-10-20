# Transactional Collections

`id_effect::stm` builds eight collections on top of `TRef`. Their operations return `Stm` values,
so they compose with `TRef` operations and with each other in one transaction, and they work
with `Stm::retry` (an operation that has to wait parks the fiber until a cell it read changes)
and `Stm::or_else` (run an alternative when the first branch would wait). Each constructor is a
transaction too: commit it once and share the handle (every type is `Clone + Send + Sync`).

| Type | Constructor | Operations | Waits (retries) when |
|------|-------------|------------|----------------------|
| `TRef<A>` | `TRef::make(a)` | `read_stm`, `write_stm`, `update_stm`, `modify_stm` | never |
| `TQueue<A>` | `TQueue::bounded(n)`, `TQueue::unbounded()` | `offer(a) -> bool` (`false` when full), `take()` | `take` on an empty queue |
| `TMap<K, V>` | `TMap::make()` | `get(&k) -> Option<V>`, `set(k, v)`, `delete(&k)` | never |
| `TSemaphore` | `TSemaphore::make(n)` | `acquire()`, `release()` | `acquire` with no permit left |
| `TSet<A>` | `TSet::make()`, `TSet::from_values(it)` | `add(a) -> bool`, `remove(&a) -> bool`, `contains`, `size`, `is_empty`, `to_vec`, `take_any()` | `take_any` on an empty set |
| `TArray<A>` | `TArray::make(vec)` | `get(i) -> Option<A>`, `set(i, a) -> bool`, `update(i, f) -> bool`, `to_vec`, `fold`, `len`, `cell(i)` | never |
| `TPriorityQueue<A: Ord>` | `TPriorityQueue::empty()`, `from_values(it)` | `offer`, `offer_all`, `take()` (smallest first), `take_option`, `take_all`, `peek()`, `size` | `take` / `peek` on an empty queue |
| `TPromise<A, E>` | `TPromise::make()` | `await_() -> Stm<A, E>`, `succeed(a) -> bool`, `fail(e) -> bool`, `complete(r)`, `poll`, `is_done` | `await_` while pending |
| `TReentrantLock` | `TReentrantLock::make()` | `acquire_read`, `release_read`, `acquire_write`, `release_write`, `read_locks`, `write_locks`, `is_locked` | an acquire another fiber blocks |

The examples below share this helper, which commits a transaction on the global runtime:

```rust
# use id_effect::{Cause, Effect, Exit};
fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
```

## TQueue: producer / consumer

`take` waits (retries) while the queue is empty. `offer` on a full bounded queue does *not* wait:
it returns `false`. Add `Stm::check` to make a waiting offer:

```rust
use id_effect::stm::{Stm, TQueue, commit};
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

## TMap and TSet: atomic with other cells

Reads and writes on a `TMap` or `TSet` and a `TRef` in one transaction commit together, and a
transaction that fails commits nothing:

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{Stm, TMap, TRef, TSet, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let users = run(commit(TMap::<String, String>::make())).expect("map");
let online = run(commit(TSet::<String>::make())).expect("set");
let logins = run(commit(TRef::make(0_u32))).expect("cell");
run(commit(users.set::<()>("alice".into(), "Alice".into()))).expect("set");

// Log in: mark online and count the login only when the user was not online yet.
let login = {
  let logins = logins.clone();
  online
    .add::<()>("alice".into())
    .flat_map(move |fresh| logins.update_stm(move |n| if fresh { n + 1 } else { n }))
};
run(commit(login.clone()))?;
run(commit(login))?;
assert_eq!(run(commit(logins.read_stm::<()>())), Ok(1));

// A failing transaction rolls back both the set and the map change.
let failing = {
  let users = users.clone();
  online
    .remove::<&str>(&"alice".to_string())
    .flat_map(move |_| users.delete(&"alice".to_string()))
    .flat_map(|()| Stm::<(), &str>::fail("audit log unavailable"))
};
assert_eq!(run(commit(failing)), Err("audit log unavailable"));
assert_eq!(run(commit(online.contains::<()>(&"alice".to_string()))), Ok(true));
assert_eq!(run(commit(users.get::<()>(&"alice".to_string()))), Ok(Some("Alice".into())));
# Ok::<(), ()>(())
```

`TSet::take_any` removes some element and waits while the set is empty.

## TSemaphore: transactional permits

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{Stm, TSemaphore, commit};
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

## TArray: one cell per element

A `TArray` has a fixed length and one `TRef` per element, so transactions that touch different
indices never conflict, and a waiting transaction wakes only when an element it read changes.
Out-of-range indices are not failures: `get` gives `None`, `set` and `update` give `false`.

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{Stm, TArray, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let balances = run(commit(TArray::make(vec![100_i64, 100]))).expect("array");
let transfer = |from: usize, to: usize, amount: i64| {
  let b = balances.clone();
  balances.get::<&str>(from).flat_map(move |bal| match bal {
    Some(bal) if bal >= amount => {
      let b2 = b.clone();
      b.update(from, move |n| n - amount)
        .flat_map(move |_| b2.update(to, move |n| n + amount))
        .map(|_| ())
    }
    Some(_) => Stm::fail("insufficient funds"),
    None => Stm::fail("no such account"),
  })
};
assert_eq!(run(commit(transfer(0, 1, 30))), Ok(()));
assert_eq!(run(commit(transfer(0, 1, 1_000))), Err("insufficient funds"));
assert_eq!(run(commit(balances.to_vec::<()>())), Ok(vec![70, 130]));
```

## TPriorityQueue: smallest first

`take` removes the smallest element by `Ord` and waits while the queue is empty; wrap elements in
`std::cmp::Reverse` for largest-first. `take_option` never waits.

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{TPriorityQueue, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let jobs = run(commit(TPriorityQueue::<(u8, &str)>::empty())).expect("queue");
let worker = {
  let jobs = jobs.clone();
  std::thread::spawn(move || run(commit(jobs.take::<()>())).expect("take")) // waits
};
run(commit(jobs.offer_all::<()>(vec![(2, "report"), (0, "page on-call"), (1, "rotate logs")])))?;
assert_eq!(worker.join().expect("join"), (0, "page on-call"));
assert_eq!(run(commit(jobs.take_all::<()>())), Ok(vec![(1, "rotate logs"), (2, "report")]));
assert_eq!(run(commit(jobs.take_option::<()>())), Ok(None));
# Ok::<(), ()>(())
```

## TPromise: complete once, await many

`await_` waits until the promise is completed, then yields the value or fails the transaction
with the promise's error. The first `succeed` / `fail` wins; later ones return `false`.

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{Stm, TPromise, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let config = run(commit(TPromise::<String, String>::make())).expect("promise");
let reader = {
  let config = config.clone();
  std::thread::spawn(move || run(commit(config.await_())).expect("await")) // waits
};
assert_eq!(run(commit(config.succeed::<()>("db=primary".into()))), Ok(true));
assert_eq!(run(commit(config.succeed::<()>("db=replica".into()))), Ok(false));
assert_eq!(reader.join().expect("join"), "db=primary");

// `or_else` gives a default instead of waiting on a pending promise.
let pending = run(commit(TPromise::<String, String>::make())).expect("promise");
let value = pending.await_().or_else(Stm::succeed("defaults".to_string()));
assert_eq!(run(commit(value)), Ok("defaults".to_string()));
```

## TReentrantLock: read/write lock owned by fibers

The lock owner is the fiber running `commit`. Any number of fibers may hold read locks while no
other fiber writes; one fiber at a time holds the write lock, and only while no other fiber reads.
Both locks are reentrant: a fiber may take them again (each acquire needs a matching release),
the writer may also read, and the only reader may upgrade to the write lock. Acquire and release
in one effect chain so both run on the same fiber:

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{Stm, TReentrantLock, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let lock = run(commit(TReentrantLock::make())).expect("lock");
let (l1, l2, l3) = (lock.clone(), lock.clone(), lock.clone());
let critical = commit(l1.acquire_write::<()>())
  .flat_map(move |_| commit(l2.acquire_write::<()>())) // reentrant: 2 holds
  .flat_map(move |held| {
    let l = l3.clone();
    commit(l3.release_write::<()>())
      .flat_map(move |_| commit(l.release_write::<()>()))
      .map(move |_| held)
  });
assert_eq!(run(critical), Ok(2));

// A try-lock: `or_else` runs when the acquire would wait.
let try_read = lock.acquire_read::<()>().map(|_| true).or_else(Stm::succeed(false));
assert_eq!(run(commit(try_read)), Ok(true)); // the lock is free again
```

STM transactions must not span I/O. To bound concurrent *effects* (permits held across I/O), use
core `id_effect::Semaphore`; hold a `TReentrantLock` across effects only in one effect chain, and
release it on every path.

## Examples

| Example | Shows |
|---------|-------|
| `077_stm_tref` … `085_stm_fail_retry` | `TRef`, `commit`, contention, retry |
| `080_stm_tmap`, `081_stm_tqueue`, `082_stm_tsemaphore`, `086_stm_collections` | `TMap`, `TQueue`, `TSemaphore` |
| `087_stm_tset` | `TSet` with a `TRef` counter, rollback, `take_any` + `or_else` |
| `088_stm_tarray` | `TArray` transfers from several threads |
| `089_stm_tpriority_queue` | `TPriorityQueue` ordering, waiting `take`, fallback queue |
| `090_stm_tpromise` | `TPromise` with several waiters, completes once |
| `091_stm_treentrant_lock` | `TReentrantLock` reentrant writer excluding readers |

Run one with `cargo run -p id_effect --example 087_stm_tset`. The integration test
`crates/id_effect/tests/stm_collections.rs` covers each type and a multi-fiber contention run.

## Summary

| Type | Purpose |
|------|---------|
| `TRef<T>` | Single mutable value |
| `TQueue<T>` | FIFO queue; `take` waits while empty |
| `TMap<K, V>` | Transactional hash map |
| `TSemaphore` | Transactional permit counter |
| `TSet<T>` | Transactional hash set |
| `TArray<T>` | Fixed-length array, one cell per element |
| `TPriorityQueue<T>` | Min-priority queue; `take` waits while empty |
| `TPromise<A, E>` | Write-once value; `await_` waits until completed |
| `TReentrantLock` | Read/write lock, reentrant per fiber |

All compose with `flat_map`, `or_else` and `retry`, and commit atomically with other STM
operations.
