# Schedule — The Retry/Repeat Policy Type

A `Schedule` is not just a number of retries or a delay. It's a *policy*: a pure step function that takes the current instant, an input and its own state, and decides whether to continue and how long to wait.

## The Model

`Schedule<Out, In>` (Effect.ts `Schedule`) is an immutable, cheaply cloneable description of a recurrence:

- **`In`** is what the schedule sees at each step: the **error** for `retry`, the **value** for `repeat`.
- **`Out`** is what it outputs: `recurs` / `spaced` / `forever` count recurrences (`u64`), `exponential` / `fibonacci` output the delay (`Duration`), `union` / `intersect` pair both outputs, `map` transforms it.
- It holds **no per-run state**. `start()` makes a fresh `ScheduleState`; `step(now, &input, state)` returns a `ScheduleStep` — the next state, the output, and the delay before the next recurrence, or "done". One schedule value can drive any number of concurrent runs.

```rust
use std::time::Instant;
use id_effect::Schedule;

let s = Schedule::<u64, ()>::recurs(2);
let mut state = s.start();
for _ in 0..3 {
    state = s.step(Instant::now(), &(), state).into_parts().0;
}
// This run is exhausted; a fresh run of the same value is not.
assert!(s.step(Instant::now(), &(), state).is_done());
assert!(!s.step(Instant::now(), &(), s.start()).is_done());
```

You rarely step a schedule by hand — `Effect::retry` and `Effect::repeat` do it — but the model explains the semantics: `recurs(n)` allows `n` *recurrences*, so `n + 1` attempts.

## Creating Schedules

```rust
use std::time::Duration;
use id_effect::Schedule;
# fn retries_on_unit<O>(_: &Schedule<O, ()>) {}

let fixed = Schedule::spaced(Duration::from_secs(1));             // 1s, 1s, 1s, …
let exponential = Schedule::exponential(Duration::from_millis(100)); // 100ms, 200ms, 400ms, …
let fibonacci = Schedule::fibonacci(Duration::from_millis(100));     // 100, 100, 200, 300, 500ms, …
let forever = Schedule::forever();                                 // no delay, never done
let three = Schedule::recurs(3);                                   // 3 recurrences, no delay
# retries_on_unit(&fixed);
# retries_on_unit(&exponential);
# retries_on_unit(&fibonacci);
# retries_on_unit(&forever);
# retries_on_unit(&three);
```

## Combining Schedules

```rust
# use std::time::Duration;
# use id_effect::Schedule;
# fn ms(n: u64) -> Duration { Duration::from_millis(n) }
# struct ApiError { transient: bool }
# impl ApiError { fn is_transient(&self) -> bool { self.transient } }
// Exponential backoff, at most 3 retries: delays 10 + 20 + 40 = 70 ms in total.
let backoff = Schedule::exponential(ms(10)).intersect(Schedule::recurs(3));

// Stop once 30 s have elapsed since the first step, whatever the count.
let bounded = Schedule::exponential(ms(100)).up_to(Duration::from_secs(30));

// Only keep retrying transient errors.
let transient = backoff.clone().while_input(|e: &ApiError| e.is_transient());
# let _: &Schedule<Duration, ApiError> = &bounded;
```

- `intersect` continues while **both** continue, with the longer delay (this is the 0.6 `compose`).
- `union` continues while **either** continues, with the shorter delay.
- `and_then` runs one schedule to completion, then the other.

## Schedule as a Value

Like effects, schedules are values: define them once, clone them freely (one `Arc`), reuse them across calls and fibers.

```rust
# use std::time::Duration;
# use id_effect::{Effect, Schedule, caps, succeed};
# struct ApiError;
# struct Response;
# struct HttpClient;
# fn make_request() -> Effect<Response, ApiError, caps!(HttpClient)> { succeed(Response) }
fn default_retry() -> Schedule<(Duration, u64), ApiError> {
    Schedule::exponential(Duration::from_millis(100))
        .intersect(Schedule::recurs(5))
        .jittered()
}

fn call_external_api() -> Effect<Response, ApiError, caps!(HttpClient)> {
    Effect::retry(make_request, default_retry())
}
```

Jitter (random delay variation) reduces thundering-herd problems when many processes retry at once. `.jittered()` scales every delay by a random factor in `[0.8, 1.2]`.
