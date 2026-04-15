# `coordination` — Stratum 9: communication between fibers

**Synchronization and messaging**: [`Deferred`](deferred.rs), [`Latch`](latch.rs), [`Queue`](queue.rs), [`Semaphore`](semaphore.rs) / [`Permit`](semaphore.rs), [`PubSub`](pubsub.rs), [`Channel`](channel.rs), [`Ref`](ref_.rs), and [`SynchronizedRef`](synchronized_ref.rs).

## What lives here

| Module | Role |
|--------|------|
| `deferred` | Single-assignment cell / promise-style handoff. |
| `latch` | Countdown / rendezvous. |
| `queue` | `Queue`, `QueueError` — MPSC-style coordination. |
| `semaphore` | `Semaphore`, `Permit` — rate limiting / backpressure. |
| `pubsub` | Broadcast to subscribers. |
| `channel` | `Channel`, `QueueChannel`, `ChannelReadError` — stream-friendly channels. |
| `ref_` | `Ref` — shared mutable cell in the effect world. |
| `synchronized_ref` | Mutex-like shared state with effectful ops. |

## What it is used for

- **Handing off** results between fibers (`Deferred`, `Latch`).
- **Backpressuring** producers (`Semaphore`, bounded `Queue`).
- **Fan-out** notification (`PubSub`) without ad hoc shared `Arc<Mutex<…>>` at app level.

## Best practices

1. **Prefer typed `Ref` / STM** over raw `Arc<Mutex<T>>` when the crate’s laws matter for your scenario.
2. **Handle `ChannelReadError`** — closed channel is a normal outcome in shutdown.
3. **Pair** semaphores with [`resource::Scope`](../resource/README.md) so permits return on exit.
4. **Avoid deadlocks** — document lock/order when combining `SynchronizedRef` with other primitives.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 9.
- [`streaming`](../streaming/README.md) — channels often feed streams.
- [`stm`](../stm/README.md) — alternative transactional state for contested data.
