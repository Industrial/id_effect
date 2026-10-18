# TRef — Transactional References

`TRef<T>` is the fundamental mutable cell in `id_effect::stm`. It holds a value that
transactions read and write.

## Creating a TRef

`TRef::make(value)` is itself a transaction (`Stm<TRef<T>, ()>`). Create long-lived cells once by
committing it, then share the handle:

```rust
use id_effect::stm::{TRef, commit};
// `run` is the Exit-to-Result helper from the chapter introduction.
# use id_effect::{Cause, Effect, Exit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }

let counter: TRef<i32> = run(commit(TRef::make(0_i32))).expect("cell");
let total: TRef<i32> = run(commit(TRef::make(0_i32))).expect("cell");
```

Creating a cell *inside* a larger transaction (`TRef::make(0).flat_map(|cell| …)`) also works, but
the cell is then local to that transaction: each restart creates a fresh one.

## Transactional Operations

Every `TRef` operation returns an `Stm` (a description, not an effect). Nothing happens until it
is committed:

| Operation | Type | Meaning |
|-----------|------|---------|
| `cell.read_stm()` | `Stm<T, E>` | read (repeated reads in one transaction see the same snapshot) |
| `cell.write_stm(v)` | `Stm<(), E>` | write, applied at commit |
| `cell.update_stm(f)` | `Stm<(), E>` | read, apply `f: Fn(T) -> T`, write |
| `cell.modify_stm(f)` | `Stm<B, E>` | `f: Fn(T) -> (B, T)`: write the new state, return `B` |

The error type `E` is chosen by the surrounding transaction. When a `TRef` operation runs on its
own, name it with a turbofish (`read_stm::<()>()`).

## Composing operations

Sequence operations with `flat_map`; the whole chain commits atomically:

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{Stm, TRef, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
# let counter: TRef<i32> = run(commit(TRef::make(0_i32))).expect("cell");
# let total: TRef<i32> = run(commit(TRef::make(0_i32))).expect("cell");
/// Increment `counter` and add its old value to `total`, atomically.
fn bump(counter: &TRef<i32>, total: &TRef<i32>) -> Stm<(), ()> {
  let (counter, total) = (counter.clone(), total.clone());
  counter.read_stm().flat_map(move |count| {
    let (counter, total) = (counter.clone(), total.clone());
    total.read_stm().flat_map(move |sum| {
      let total = total.clone();
      counter.write_stm(count + 1).flat_map(move |()| total.write_stm(sum + count))
    })
  })
}

// modify_stm returns something computed from the old value (here: the previous count).
# for _ in 0..3 {
#     run(commit(bump(&counter, &total))).expect("bump");
# }
let previous = run(commit(counter.modify_stm::<_, (), _>(|n| (n, n * 10))));
# assert_eq!(previous, Ok(3));
# assert_eq!(run(commit(total.read_stm::<()>())), Ok(3));
```

## Sharing TRefs

`TRef` is `Clone + Send + Sync`, and a clone is another handle to the same cell. Clone it into
each thread or fiber; no `Arc<Mutex<_>>` is needed:

```rust
# use id_effect::{Cause, Effect, Exit};
# use id_effect::stm::{TRef, commit};
# fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
#   match eff.block_on() {
#     Exit::Success(a) => Ok(a),
#     Exit::Failure(Cause::Fail(e)) => Err(e),
#     Exit::Failure(_) => panic!("effect died or was interrupted"),
#   }
# }
let hits = run(commit(TRef::make(0_u32))).expect("cell");
let handles: Vec<_> = (0..2).map(|_| {
  let hits = hits.clone();
  std::thread::spawn(move || {
    for _ in 0..1_000 {
      run(commit(hits.update_stm::<(), _>(|n| n + 1))).expect("update");
    }
  })
}).collect();
for h in handles { h.join().expect("join"); }
assert_eq!(run(commit(hits.read_stm::<()>())), Ok(2_000));
```

Full example: [`crates/id_effect/examples/084_stm_tref_compose.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/examples/084_stm_tref_compose.rs) (`cargo run -p id_effect --example 084_stm_tref_compose`); the minimal read/write version is `077_stm_tref`.

## `TRef` vs. `Mutex<T>`

| Property | TRef | Mutex |
|----------|------|-------|
| Composable across updates | ✓ | ✗ |
| Deadlock-free | ✓ | ✗ |
| Works with I/O inside | ✗ | ✓ |
| Overhead | restart cost under contention; commits serialise on one global lock | lock/unlock cost |

Use `TRef` for short, composable state mutations. Use `Mutex` when you need to hold a lock across I/O (though ideally you redesign to avoid that).
