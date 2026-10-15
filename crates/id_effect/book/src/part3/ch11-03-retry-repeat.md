# retry and repeat — Applying Policies

`Schedule` is a policy description. `Effect::retry` and `Effect::repeat` are the two operations that apply it.

## Effects Run Once, So Pass a Factory

A 0.7 `Effect` is a single-shot value (its steps are `FnOnce`): it cannot be re-run. So `retry` and `repeat` are associated functions that take the effect's **factory** — an `Fn() -> Effect`, called once per attempt, when the attempt starts:

```rust,ignore
Effect::retry(move || fetch(id), schedule)
```

There are no free `retry` / `repeat` functions and no `.retry(..)` method on a built effect.

## retry: On Failure, Try Again

`retry` runs `make()`; after each **typed failure** it steps the schedule with the error. On "continue" it sleeps the delay and tries again; on "done" it fails with the last error. **Defects and interrupts are never retried.** From `examples/054_schedule_retry.rs`:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use id_effect::{Effect, Exit, Schedule, fail, succeed, suspend};

let n = Arc::new(AtomicU32::new(0));
let n2 = Arc::clone(&n);
let eff: Effect<u8, &'static str> = Effect::retry(
    move || {
        let n = Arc::clone(&n2);
        suspend(move || if n.fetch_add(1, Ordering::SeqCst) < 2 { fail("retry me") } else { succeed(42) })
    },
    Schedule::recurs(5),
);
assert_eq!(eff.block_on(), Exit::succeed(42));
assert_eq!(n.load(Ordering::SeqCst), 3);
```

When the schedule is exhausted the last error is the outcome (`examples/055_schedule_exhausted.rs`):

```rust
use id_effect::{Cause, Effect, Exit, Schedule, fail};

let eff: Effect<(), &'static str> = Effect::retry(|| fail("nope"), Schedule::recurs(1));
assert_eq!(eff.block_on(), Exit::Failure(Cause::Fail("nope")));
```

## Retrying Only Some Errors

Not every error is retriable. The schedule's input is the error, so filter with `while_input`:

```rust,ignore
let eff = Effect::retry(
    move || api_call(id),
    Schedule::exponential(Duration::from_millis(100))
        .intersect(Schedule::recurs(5))
        .while_input(|e: &ApiError| e.is_transient()),
);
```

A permanent error (404, permission denied) stops the schedule at once and is returned.

## retry_or_else: Fall Back When Exhausted

```rust,ignore
let eff = Effect::retry_or_else(
    move || primary(id),
    Schedule::recurs(3),
    move |last_err, _last_out| secondary(id, last_err),
);
```

## repeat: On Success, Run Again

`repeat` runs `make()`; after each **success** it steps the schedule with the value. On "continue" it sleeps and runs again; on "done" it succeeds with the **schedule's last output**. The first failure stops the repetition and is returned. From `examples/052_schedule_repeat_n.rs`:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use id_effect::{Effect, Exit, Schedule, sync};

let calls = Arc::new(AtomicU32::new(0));
let c = Arc::clone(&calls);
let eff: Effect<u64, ()> = Effect::repeat(
    move || {
        let c = Arc::clone(&c);
        sync(move || c.fetch_add(1, Ordering::SeqCst) + 1)
    },
    Schedule::recurs(2),
);
assert_eq!(eff.block_on(), Exit::succeed(2)); // the schedule's output: 2 recurrences
assert_eq!(calls.load(Ordering::SeqCst), 3);  // 1 run + 2 repeats
```

Use cases: poll for job completion, send heartbeats, refresh a cache on an interval. To stop when the value satisfies a condition, bound the schedule with `while_input(|status| !status.is_ready())`. `repeat_or_else(make, schedule, |err, last_out: Option<_>| ..)` recovers from a failure.

## Composition with Other Operations

`retry` and `repeat` return effects; they compose like everything else:

```rust,ignore
// Retry each call, then repeat the whole batch every minute.
let continuous = Effect::repeat(
    move || Effect::retry(move || process_batch(), Schedule::exponential(ms(100)).intersect(Schedule::recurs(3))),
    Schedule::spaced(Duration::from_secs(60)),
);
```

## Observing Retries

To log each failed attempt, do it inside the factory (it is called once per attempt) or tap the error before it reaches the schedule:

```rust,ignore
Effect::retry(
    move || risky_call().map_err(|e| { tracing::warn!(error = ?e, "attempt failed"); e }),
    schedule,
)
```

Delays are slept on the fiber's executor clock, so all of this is testable in virtual time — see [Virtual Time](./ch11-04-clock-injection.md).
