# `scheduling` — Stratum 10: time, clocks & policies

**Temporal policies** and **wall-clock abstraction**: [`Duration`](duration.rs), [`UtcDateTime` / `ZonedDateTime` / `AnyDateTime`](datetime.rs), [`Clock`](clock.rs) ([`LiveClock`](clock.rs), [`TestClock`](clock.rs)), and [`Schedule`](schedule.rs) with **repeat** / **retry** combinators.

## What lives here

| Module | Role |
|--------|------|
| `duration` | `Duration`, `DurationParseError` — spans and parsing. |
| `datetime` | Instants, zones, `TimeUnit`, `timezone` helper. |
| `clock` | `Clock` trait + test/live implementations — inject time in `Effect`. |
| `schedule` | `Schedule`, `ScheduleDecision`, `repeat`, `retry`, `forever`, clock+interrupt variants. |

## What it is used for

- **Retries** with backoff and jitter policies (via `retry*`).
- **Loops** and polling (`repeat*`, `forever`) with optional interrupt integration.
- **Deterministic tests** by swapping `TestClock` for `LiveClock`.

## Best practices

1. **Inject `Clock`** through `R` instead of calling `std::time::Instant::now()` deep in library code.
2. **Combine** `CancellationToken` with `*_and_interrupt` schedule variants for clean shutdown.
3. **Keep schedules pure** in their decision logic — side effects belong in the `Effect` you pass to `repeat`/`retry`.
4. **Parse durations** once at config load — surface `DurationParseError` early.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 10.
- [`concurrency`](../concurrency/README.md) — interrupts pair with schedules.
- [`testing::run_test_with_clock`](../testing/README.md) — harness with clock control.
