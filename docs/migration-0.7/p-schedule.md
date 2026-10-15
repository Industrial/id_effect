# `Schedule<Out, In>` and `Effect::retry` / `Effect::repeat`

0.7 replaces the untyped, stateful `Schedule` enum (`fn next(&mut self, ..)`) and the free
`retry*` / `repeat*` functions with an immutable, typed `Schedule<Out, In>` (Effect.ts
`Schedule`) and associated functions on `Effect`.

```rust
// 0.6
retry(|| fetch(id), Schedule::exponential(Duration::from_millis(10)).compose(Schedule::recurs(3)))
repeat(|| poll(), Schedule::spaced(Duration::from_secs(1)))

// 0.7
Effect::retry(move || fetch(id), Schedule::exponential(ms(10)).intersect(Schedule::recurs(3)))
Effect::retry_or_else(make, schedule, |last_err, last_out| fallback(last_err))
Effect::repeat(|| poll(), Schedule::spaced(secs(1)).intersect(Schedule::recurs(9)))
Effect::repeat_or_else(make, schedule, |err, last_out: Option<_>| recover(err))
```

- A `Schedule` holds no per-run state: `start()` makes a fresh `ScheduleState` and
  `step(now, &input, state)` returns the next state, an output and the delay (`None` = done).
  One schedule value can drive many runs; clone it freely (one `Arc`).
- `In` is what the schedule sees: the error for `retry`, the value for `repeat`
  (`while_input(|e| e.is_transient())`). `Out` is its output: `recurs` / `spaced` / `forever`
  count recurrences, `exponential` / `fibonacci` output the delay, `union` / `intersect` pair
  both outputs, `map` transforms it.
- Combinators: `recurs`, `forever`, `spaced`, `exponential`, `exponential_by(base, factor)`,
  `fibonacci`, `jittered` (`[0.8, 1.2]`), `jittered_with(min, max, JitterSource)`, `union`
  (either continues, shorter delay), `intersect` (both continue, longer delay; replaces
  `compose`), `and_then`, `while_input`, `up_to`, `map`, `from_fn`.
- `recurs(n)` allows `n` recurrences, so `n + 1` attempts. `exponential(10ms)` intersected with
  `recurs(3)` sleeps 10 + 20 + 40 = 70 ms in total.
- `retry` / `repeat` take the effect's factory (`Fn() -> Effect`), called once per attempt when
  the attempt starts, because a 0.7 `Effect` value runs once. Only typed failures are retried;
  defects and interrupts pass through.
- Delays are slept on the fiber's executor clock (virtual on `TestExecutor`), and are
  `std::time::Duration`. The `*_with_clock` / `*_with_clock_and_interrupt` variants are gone:
  the clock is the executor's and interruption is the fiber's.
