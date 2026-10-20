# Software Transactional Memory — Optimistic Concurrency

> **Module:** STM lives in core, in **`id_effect::stm`** (`Stm`, `TRef`, `TQueue`, `TMap`,
> `TSemaphore`, `Txn`, `Outcome`, `commit`, `atomically`). It is always compiled and needs no
> feature. From 0.6 to 0.10 it lived in the maintenance-tier `id_effect_fp` crate; the
> `id_effect_fp` paths remain as deprecated aliases in 0.11 and are removed in 0.12.

Shared mutable state is hard. Mutexes work but compose poorly: lock two mutexes in the wrong order and you deadlock. Lock them separately and you get torn reads. Lock the whole world and you serialise unnecessarily.

Software Transactional Memory (STM) takes a different approach: every operation on shared state runs inside a *transaction*. Transactions commit atomically or restart. No explicit locks. No deadlocks. No torn reads.

The model in `id_effect::stm`:

- An `Stm<A, E>` is a *description* of a transaction that produces `A` or fails with `E`. Build
  it from `TRef` operations and compose with `map`, `flat_map` and `or_else`.
- `commit(stm)` (alias `atomically`) turns it into an `Effect<A, E, R>`, an ordinary 0.7
  effect: run it with `.block_on()` at the edge or compose it with other effects. Running it
  executes the transaction and restarts it when validation finds that a cell it read has changed.
- Commits are serialised behind one global lock; transaction bodies run without it.
- A transaction that *retries* parks its fiber until another commit writes one of the cells it
  read (Effect.ts `STM.retry`); nothing spins and no thread is blocked.

The snippets below use a small helper from the examples that runs an effect and turns its `Exit`
into a `Result`:

```rust
use id_effect::{Cause, Effect, Exit};

fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(_) => panic!("effect died or was interrupted"),
  }
}
```

Every snippet in this chapter is an excerpt of a runnable, asserted example in
`crates/id_effect/examples/` (`083_stm_transfer` … `086_stm_collections`, plus the smaller
`077_stm_tref` … `082_stm_tsemaphore`), so they are compile-checked by
`cargo check -p id_effect --examples`.
