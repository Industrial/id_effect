# What Are Fibers? — Lightweight Structured Tasks

A fiber is an effect-managed task. It's lighter than an OS thread and safer than a raw `tokio::spawn`.

## Fibers vs. Raw Tasks

```rust,no_run
# async fn do_something() {}
# fn background_work() -> id_effect::Effect<u8, ()> { id_effect::succeed(1) }
// Raw tokio::spawn — fire and forget.
// Who owns this? What happens if it panics? Who cleans up?
tokio::spawn(async {
    do_something().await;
});

// A fiber — explicit lifecycle.
let program = background_work()
    .fork()                         // Effect<FiberHandle<A, E>, Never, R>
    .widen()                        // fit the fork step into a fallible chain
    .flat_map(|handle| handle.join()); // re-raises the child's outcome
```

`fork` is itself an effect: it starts the child when it runs and yields a `FiberHandle<A, E>`. `handle.join()` waits for the child and re-raises its outcome (success, typed failure, defect or interrupt) in the parent. A panic in the child becomes `Cause::Die`, never a lost task.

```rust
use id_effect::{Effect, Exit, succeed};

let child: Effect<u8, ()> = succeed(5);
let program: Effect<u8, ()> = child.fork().widen().flat_map(|h| h.join());
assert_eq!(program.block_on(), Exit::succeed(5));
```

## Structured Concurrency

The key property of fibers is a *structured* lifecycle:

- A child forked with `fork` is **interrupted and awaited when its parent exits** (success, failure or interrupt). It cannot outlive its parent by accident.
- `fork_daemon` is the explicit opt-out: the daemon outlives the fiber that forked it.
- `Scope::fork_in(eff)` ties a fiber to a [`Scope`](./ch10-02-scopes-finalizers.md) instead: it is interrupted when the scope closes.
- Panics and failures surface through `join`, not into the void.

So when `process_batch` completes, all its helper fibers have completed too — or been interrupted, with their finalizers run.

Dropping a `FiberHandle` does **not** interrupt the fiber; the parent-exit rule above still applies.

## FiberId

Each fiber has a `FiberId` (the one its `Cause::Interrupt` carries). `FiberHandle::id()` gives a child's id; inside an effect, the current fiber is on the `Ctx`:

```rust
use id_effect::{Effect, FiberId};

fn current_fiber_id<E: Send + 'static>() -> Effect<FiberId, E> {
    Effect::from_ctx(|ctx| async move { Ok(ctx.fiber().id()) })
}
# assert!(current_fiber_id::<()>().block_on().into_result().is_ok());
```

## FiberHandle

`FiberHandle<A, E>` is the control interface for a forked fiber:

| Method | What it does |
|---|---|
| `join()` | effect: wait and re-raise the child's outcome |
| `interrupt()` | effect: request interruption, wait, and succeed with the child's `Exit` |
| `interrupt_now()` | request interruption without waiting |
| `is_done()` | `true` once the outcome is available |
| `id()` / `fiber()` | the fiber's id and its `FiberContext` |
| `join_future()` | the child's `Exit` as a plain `Future` (for async code at the edge) |

Unlike `tokio::task::JoinHandle`, interrupting a fiber runs its finalizers and its children's before `interrupt()` returns.
