# Built-in Schedules — exponential, fibonacci, spaced, recurs

id_effect provides the common schedules and combinators of Effect.ts. This section catalogs them with typical use cases. Durations are `std::time::Duration`.

## Constructors

| Constructor | Delays | Output | Done |
|---|---|---|---|
| `Schedule::recurs(n)` | none | recurrence count | after `n` recurrences |
| `Schedule::forever()` | none | recurrence count | never |
| `Schedule::spaced(d)` | `d`, `d`, `d`, … | recurrence count | never |
| `Schedule::exponential(base)` | `base`, `2·base`, `4·base`, … (saturates at `Duration::MAX`) | the delay | never |
| `Schedule::exponential_by(base, factor)` | `base · factorᵏ` | the delay | never |
| `Schedule::fibonacci(one)` | `one`, `one`, `2·one`, `3·one`, `5·one`, … | the delay | never |
| `Schedule::from_fn(init, step)` | whatever `step` returns | yours | yours |

**exponential** is the standard backoff for network retries: it backs off quickly and gives the downstream service time to recover. **fibonacci** grows more gradually, so you get more early attempts before the delay gets large. **spaced** is a fixed interval — polling, heartbeats. **forever** / **recurs** have no delay; combine them with a delaying schedule or use them to bound one.

Most constructors never finish on their own, so always bound them.

## Limiters

```rust,ignore
schedule.intersect(Schedule::recurs(5))        // at most 5 recurrences
schedule.up_to(Duration::from_secs(30))        // stop once 30 s have elapsed since the first step
schedule.while_input(|e: &ApiError| e.is_transient()) // continue only while the input matches
```

There is no `max_delay` cap combinator; for a capped backoff, write it with `Schedule::from_fn` or bound the total time with `up_to`.

## Jitter

```rust,ignore
schedule.jittered()                                   // each delay × random factor in [0.8, 1.2]
schedule.jittered_with(0.5, 1.5, JitterSource::seeded(42)) // custom range, reproducible
```

Jitter prevents retry storms: when 1000 clients hit a failing service and retry in lockstep they create a "thundering herd"; jitter spreads them out. `jittered` seeds its generator per run from `JitterSource::entropy()`; tests pass `JitterSource::seeded(n)` to `jittered_with` for a reproducible sequence.

## Composition

| Combinator | Continues while | Delay | Output |
|---|---|---|---|
| `a.intersect(b)` | both continue | the longer one | `(a_out, b_out)` |
| `a.union(b)` | either continues | the shorter one | `(a_out, b_out)` |
| `a.and_then(b)` | `a`, then `b` | the current one's | the current one's |
| `a.map(f)` | `a` | `a`'s | `f(a_out)` |

```rust,ignore
let bounded_backoff = Schedule::exponential(Duration::from_millis(100))
    .intersect(Schedule::recurs(5))
    .jittered()
    .up_to(Duration::from_secs(10));
```

`intersect` is the "both must agree to continue" combination: the first schedule that says "done" ends the whole thing.
