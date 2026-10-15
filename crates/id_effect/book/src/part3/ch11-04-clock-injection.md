# Virtual Time — The Executor Clock

`Schedule::exponential(100ms)` intersected with `recurs(3)` takes 700 ms of real time per test run. Multiply by hundreds of tests and your suite takes minutes. In 0.7 the fix is not a clock capability you inject: **time belongs to the executor**.

## One Clock per Executor

Every fiber runs on an `Executor`, and the executor owns the clock (`Executor::sleep` and `Executor::now`). Everything time-based in id_effect uses the running fiber's executor:

- `sleep(d)`, `Effect::timeout` / `timeout_fail`;
- the delays of `Effect::retry` / `Effect::repeat` and of `supervised` restart policies;
- `Pool` / `Cache` TTLs.

In production that is `TokioExecutor` (real tokio timers; it is what `Effect::block_on` and `Runtime::global()` use). In tests it is **`TestExecutor`**, whose clock is virtual: it starts at the wall-clock instant of `TestExecutor::new()` and moves **only** when you call `advance`. Business code is identical in both — it never names a clock.

There is no `Clock` trait, `LiveClock` or `TestClock` in 0.7, and no `*_with_clock` variants: they were removed with the 0.6 kernel.

## Driving a TestExecutor

```rust,ignore
use std::sync::Arc;
use id_effect::TestExecutor;

let exec = TestExecutor::new();
let handle = program.fork_root(Arc::new(exec.clone())); // spawn the root fiber on it
exec.run_until_stalled();             // poll every queued task until none is queued
exec.advance(Duration::from_secs(1)); // move virtual time, firing due sleeps in order
assert!(handle.is_done());
```

| Method | What it does |
|---|---|
| `TestExecutor::new()` | a fresh single-threaded executor with a virtual clock (clones share it) |
| `eff.fork_root(Arc::new(exec.clone()))` | start `eff` (with `R = HNil`) as a root fiber; returns its `FiberHandle` |
| `run_until_stalled()` | poll queued tasks (FIFO) until none is queued; returns how many polls ran |
| `advance(d)` | move the clock by `d`, firing due sleeps in deadline order and running what they wake |
| `next_timer_deadline()` / `pending_timers()` | inspect pending sleeps |
| `now()` (via `Executor`) | the virtual instant |

Nothing busy-polls: a sleeping fiber is a registered timer, so a thousand ten-second sleeps finish on one `advance` in microseconds.

## Example: a Timeout in Virtual Time

From the `Effect::timeout` docs:

```rust
use std::sync::Arc;
use std::time::Duration;
use id_effect::{Effect, TestExecutor, sleep};

let exec = TestExecutor::new();
let slow: Effect<u8> = sleep(Duration::from_secs(60)).map(|()| 1);
let h = slow.timeout(Duration::from_secs(10)).fork_root(Arc::new(exec.clone()));
exec.advance(Duration::from_secs(9));
assert!(!h.is_done());
exec.advance(Duration::from_secs(1));
assert!(h.is_done(), "timed out at virtual 10 s");
```

## Example: Exact Backoff Timing

From `crates/id_effect/tests/schedule.rs`: exponential(10 ms) with 3 retries is done at exactly 70 ms of virtual time, and not one nanosecond earlier.

```rust,ignore
let eff: Effect<usize, String> = Effect::retry(
    flaky(&n, usize::MAX), // always fails, counting attempts in `n`
    Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)),
);
let exec = TestExecutor::new();
let h = eff.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
for step in [ms(10), ms(20), ms(40) - Duration::from_nanos(1)] {
    exec.advance(step);
}
assert!(!h.is_done());
assert_eq!(n.load(Ordering::SeqCst), 3);
exec.advance(Duration::from_nanos(1));
assert!(h.is_done());
assert_eq!(n.load(Ordering::SeqCst), 4); // 1 attempt + 3 retries
```

A handy helper advances from deadline to deadline until the root is done, so tests don't hard-code delays:

```rust,ignore
fn run_virtual<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> (FiberHandle<A, E>, Duration) {
    let exec = TestExecutor::new();
    let start = exec.now();
    let h = eff.fork_root(Arc::new(exec.clone()));
    exec.run_until_stalled();
    while !h.is_done() {
        let deadline = exec.next_timer_deadline().expect("stalled with no timer pending");
        exec.advance(deadline.saturating_duration_since(exec.now()));
    }
    (h, exec.now() - start)
}
```

Once `h.is_done()`, `h.join_future()` is ready on its first poll and yields the `Exit`.

## Reading the Time in Your Own Code

If business logic needs "now", read it from the executor so tests control it too:

```rust,ignore
fn now<E: Send + 'static>() -> Effect<std::time::Instant, E> {
    Effect::from_ctx(|ctx| async move { Ok(ctx.fiber().executor().now()) })
}
```

For wall-clock calendar values (`UtcDateTime`, time zones) see `id_effect::scheduling::datetime`; they are plain data, so pass them in as inputs or capabilities when a test needs to fix them. More on testing with `TestExecutor` in [Testing](../part4/ch15-00-testing.md).
