# Coordination primitives: `Deferred`, `Semaphore`, `Latch`, `Queue`, `Channel`, `PubSub`, `Ref`, `SynchronizedRef`, `SubscriptionRef`, `Supervisor`

0.7 ports the coordination primitives to the fiber kernel. A waiting operation (acquire, take,
offer, wait) suspends the fiber on a waker; no thread is blocked, and nothing is built on tokio
`watch`/`Notify`/`Semaphore`/`Mutex` or flume any more.

```rust
// 0.6
let r = Ref::make(0).run(&mut ())?;           // constructors were effects
let sem = Semaphore::make(2).run(&mut ())?;
let q = Queue::<u32>::bounded(8).run(&mut ())?;
let d = Deferred::<u8, E>::make().run(&mut ())?;

// 0.7
let r = Ref::new(0);                           // plain constructors
let sem = Semaphore::new(2);
let q = Queue::<u32>::bounded(8);
let d = Deferred::<u8, E>::new();
let l = Latch::new();                          // `Latch::with_state(true)` starts open
let s = SynchronizedRef::new(0);
let sr = SubscriptionRef::new(0);
let ps = PubSub::<u32>::bounded(16);
let ch = Channel::<u32>::duplex_unbounded();
let sup = Supervisor::new();
```

- **Constructors are plain functions**; the free-function forms (`ref_::make`, `latch::open`,
  `semaphore::acquire`, …) are gone. Operations stay lazy effects and are generic over the
  error type and environment where they cannot fail (`r.get::<MyErr, _>()`, or let inference
  pick them).
- **Interruption.** Interrupting a waiting fiber removes it from the primitive. A permit or a
  queue item handed to it but not yet observed goes back (`Semaphore`, `Queue`, `take_n`
  partial results included); an interrupted offer stores nothing.
- **`Deferred`**: `wait()` re-raises the stored cause as is (`Effect<A, E, R>`, not
  `Effect<A, Cause<E>>`); `interrupt()` uses the calling fiber's id. `complete(eff)` now
  stores `eff`'s outcome **whether it succeeds or fails** (Effect.ts); 0.6 returned the failure
  without completing. `try_done(exit)` joins `try_succeed`/`try_fail_cause`; `wait_future()`
  returns `Exit<A, E>`.
- **`Semaphore`**: `acquire()`/`acquire_n(n)` return an owned `Permit` that releases on drop
  (0.6 `acquire_owned`). The 0.6 `acquire` under a `Scope` environment is
  `acquire_scoped(&scope)`. Prefer `with_permits(n, eff)` / `with_permit(eff)`, which release
  however `eff` ends. Waiters are FIFO: a large `acquire_n` at the front blocks later small ones.
- **`Queue`**: on a full **bounded** queue `offer` now suspends until a slot frees up
  (Effect.ts back-pressure); the 0.6 non-suspending `offer` is `try_offer`, and the 0.6
  `offer_all` (returning the overflow) is `try_offer_all`. `offer_all` suspends on a bounded
  queue and returns the items that were not stored, including the ones offered after
  `shutdown` (0.6 dropped those silently). The `take_*` family returns `Vec<A>` instead of
  `Chunk<A>`. Shutdown semantics are unchanged: buffered items stay readable, then takers fail
  with `QueueError::Disconnected`; suspended offers return `false`.
- **`PubSub`**: `subscribe(&scope)` takes the scope explicitly (0.6 ran under a `Scope`
  environment) and returns a per-subscriber `Queue` with the hub's strategy. A bounded hub's
  `publish` suspends until every subscriber has room; `size`/`is_full` describe the fullest
  subscriber. `publish` still returns `false` with no subscriber.
- **`SubscriptionRef`**: `subscribe(&scope)` likewise; the queue gets the current value, then
  every later value in order with no gap (writes and subscriptions are serialized). `modify`
  is new.
- **`SynchronizedRef`**: serialized through a one-permit `Semaphore` instead of a tokio mutex;
  an interrupted effectful update releases the lock and leaves the value unchanged.
- **`Latch`**: adds `close`, `release` (wake the current waiters without opening) and
  `when_open(eff)`.
- **`Channel`** is the queue-backed channel only, `Channel<Out, In = Out>`
  (`duplex_unbounded`, `duplex_bounded`, `from_queue_and_map`, `map_in`, `map_out`,
  `flat_map_out`, `pipe`, `read`, `write`, `shutdown`); `read()` yields `None` once the queue is
  shut down and drained. The stream-, sink- and fold-backed variants (`from_stream`,
  `from_sink`, `from_fold`, `to_stream`, `to_sink`, `consume_stream`) move with the streaming
  migration (`m-streams`).
- **`Supervisor`** (from `concurrency::supervisor`) owns a `Scope`: `spawn(policy, make)` forks
  the `supervised(policy, make)` loop into it, `shutdown()` (or closing a parent it was
  `attach_to`'d) interrupts every supervised fiber. The `CancellationToken` and the explicit
  clock are gone (interruption replaces cooperative cancellation; delays use the executor
  timer, so they are virtual on `TestExecutor`). The restart policies take a fixed `delay`
  instead of a `Schedule` until the next `Schedule` is wired in. Defects are restarted like
  typed failures; an interrupt never is. `RestartIntensity` is unchanged.
