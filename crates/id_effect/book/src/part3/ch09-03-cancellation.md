# Cancellation — Interrupting Fibers

Cancellation in async code is notoriously hard to get right. In id_effect it is a property of the fiber: you **interrupt** a fiber, and the runtime stops it at its next step, runs its finalizers, interrupts its children, and only then reports it done.

## Interruption Points

A fiber is interrupted wherever it is suspended — sleeping, waiting on a `Queue`, a `Semaphore`, a `Deferred`, a joined fiber — and at the runloop's step boundaries. There is no cancellation token to thread through your code and no `check_interrupt` to sprinkle in loops.

A pure Rust loop *inside a single* `sync(..)` closure is not interruptible (it is one step). Split long CPU work into effect steps, or `yield_fiber()` between chunks:

```rust,ignore
use id_effect::{Effect, Never, succeed, sync, yield_fiber};

fn process_all(chunks: Vec<Chunk>) -> Effect<(), Never> {
    chunks.into_iter().fold(succeed(()), |acc, chunk| {
        acc.then(sync(move || process_chunk(chunk))).then(yield_fiber())
    })
}
```

## Interrupting via FiberHandle

```rust,ignore
let program = background_work().fork().widen().flat_map(|handle| {
    wait_for_shutdown_signal().then(handle.interrupt()) // Effect<Exit<A, E>, ..>
});
```

`interrupt()` requests interruption **and waits**: when it succeeds, the child's finalizers and its own children have completed, and its value is the child's `Exit` (normally `Exit::Failure(Cause::Interrupt(id))`). `interrupt_now()` only sends the request.

Interruption also happens structurally, without any handle:

- a parent's exit interrupts and awaits its `fork`ed children;
- closing a `Scope` interrupts the fibers forked into it (`Scope::fork_in`), and `Supervisor::shutdown` does the same for supervised fibers;
- the losers of `race` / `timeout` and the siblings of a failed `zip_par` / `all` are interrupted;
- at the edge, `Effect::block_on` interrupts and awaits the root's children before returning.

## Graceful Shutdown

Because finalizers run on interruption, graceful shutdown is "interrupt the top of the tree and wait":

```rust,ignore
let workers: Vec<Effect<(), AppError>> = (0..4).map(worker).collect();
let server = all(workers, Concurrency::Unbounded);

// Stop everything when the signal arrives (both sides return `Vec<()>` here).
let program = server.race_first(shutdown_signal().map(|()| Vec::new()));
```

When `shutdown_signal()` completes first, `race_first` interrupts `server`, which interrupts every worker; each worker's scopes and `ensuring` finalizers run before the race returns.

## Uninterruptible Regions

Some operations shouldn't be interrupted midway (writing a transaction, handing off a resource). Mark them uninterruptible; an interrupt that arrives inside is honoured when the region exits:

```rust,ignore
let committed = begin_transaction()
    .then(insert_records())
    .then(commit())
    .uninterruptible();
```

- `.interruptible()` re-opens a region inside an uninterruptible one (e.g. a cancellable wait).
- `uninterruptible_mask(|restore| ..)` runs a block uninterruptibly except for the parts you wrap in `restore.apply(eff)`, which get the caller's interruptibility back.
- `acquire_use_release` and `Effect::scoped` already do this for you: acquisition and release are uninterruptible, the use keeps the caller's interruptibility ([Chapter 10](./ch10-03-acquire-release.md)).

Use uninterruptible regions sparingly — they delay shutdown, and a masked effect delays its own `timeout` too.
