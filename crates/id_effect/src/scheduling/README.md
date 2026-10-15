# `scheduling` — time values and schedules

| Module | Items |
|--------|-------|
| `duration` | `Duration` helpers (`millis`, `seconds`, `decode`, `clamp`, …), `DurationParseError`. |
| `datetime` | `UtcDateTime`, `ZonedDateTime`, `AnyDateTime`, `TimeUnit`, `timezone`. |
| `schedule` | `Schedule<Out, In>` and `Effect::{retry, retry_or_else, repeat, repeat_or_else}`. |

A `Schedule` is an immutable value: `start()` gives a fresh state and `step(now, input, state)`
returns the next state, an output and the delay before the next recurrence (or done). One value
can drive any number of independent runs. Constructors: `recurs`, `spaced`, `exponential`,
`exponential_by`, `fibonacci`, `forever`, `from_fn`; combinators: `intersect`, `union`,
`and_then`, `up_to`, `while_input`, `map`, `jittered` / `jittered_with`.

Effects are one-shot, so `retry` and `repeat` take a factory that builds a fresh attempt:

```rust
use id_effect::{Effect, Exit, Schedule, succeed};
use std::time::Duration;

let eff: Effect<u8, &'static str> = Effect::retry(
  || succeed(42),
  Schedule::exponential(Duration::from_millis(10)).intersect(Schedule::recurs(3)),
);
assert_eq!(eff.block_on(), Exit::succeed(42));
```

Delays sleep on the executor clock, so tests run schedules in virtual time on a
`TestExecutor` (`advance`, `next_timer_deadline`); see `crates/id_effect/tests/schedule.rs`.
There is no `Clock` capability: the 0.6 `Clock` / `LiveClock` / `TestClock` and the free
`retry` / `repeat` / `forever` functions were removed in 0.7.
