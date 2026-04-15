# TestClock — Deterministic Time in Tests

`TestClock` was introduced in [Clock Injection](../part3/ch11-04-clock-injection.md) from a scheduling perspective. This section focuses on how to use it in tests — specifically with `run_test_with_clock` and multi-step scenarios.

## The Problem with Real Time in Tests

```rust
// This test takes 7 seconds to run
#[test]
fn retry_exhaustion_slow() {
    let eff = failing_call()
        .retry(Schedule::exponential(Duration::from_secs(1)).take(3));
    let exit = run_blocking(eff, ());
    assert!(matches!(exit, Exit::Failure(_)));
}
```

Multiply this by dozens of tests and your suite is unusable. `TestClock` makes it instant.

## run_test_with_clock

```rust
use id_effect::{run_test_with_clock, TestClock};

#[test]
fn retry_exhaustion_fast() {
    let result = run_test_with_clock(|clock| {
        let eff = failing_call()
            .retry(Schedule::exponential(Duration::from_secs(1)).take(3));

        // Fork the effect
        let handle = eff.fork();

        // Drive time forward to trigger each retry
        clock.advance(Duration::from_secs(1));
        clock.advance(Duration::from_secs(2));
        clock.advance(Duration::from_secs(4));

        // Collect the result
        handle.join()
    });

    assert!(matches!(result, Exit::Failure(_)));
}
```

`run_test_with_clock` creates a `TestClock`, injects it into the effect environment, and calls your closure with a handle to the clock. You advance time; the runtime processes sleep effects that become due.

## TestClock API

```rust
let clock = TestClock::new();

// Read the current (fake) time — starts at Unix epoch
let now: UtcDateTime = clock.now();

// Advance by a duration
clock.advance(Duration::from_millis(500));

// Jump to an absolute time
clock.set_time(UtcDateTime::from_unix_secs(1_700_000_000));

// How many sleeps are currently waiting?
let pending: usize = clock.pending_sleeps();
```

`pending_sleeps()` is useful in tests to assert that an effect is blocked on a timer rather than having silently completed or failed.

## Testing Scheduled Work

```rust
#[test]
fn cron_job_runs_every_minute() {
    run_test_with_clock(|clock| {
        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();

        let job = effect!(|_r: &mut ()| {
            c.fetch_add(1, Ordering::Relaxed);
        })
        .repeat(Schedule::fixed(Duration::from_secs(60)));

        let _handle = job.fork();

        // Advance through 3 minutes
        clock.advance(Duration::from_secs(60));
        clock.advance(Duration::from_secs(60));
        clock.advance(Duration::from_secs(60));

        succeed(counter.load(Ordering::Relaxed))
    });

    // Verify 3 executions happened
}
```

## Time and Race Conditions

`TestClock` is deterministic — time moves only when you call `advance`. This means tests that use `TestClock` have no time-based race conditions: the scheduler runs wake-up callbacks synchronously when you advance.

If your effect spawns multiple fibers that all sleep, advancing time wakes all fibers whose sleep deadline has passed, in a consistent order.

## Combining TestClock with Fake Services

```rust
#[test]
fn rate_limiter_enforces_window() {
    let fake_store = InMemoryRateLimitStore::new();
    let env = ctx!(RateLimitStoreKey => Arc::new(fake_store));

    run_test_with_clock_and_env(env, |clock| {
        let eff = effect!(|_r: &mut Deps| {
            // Should succeed (first request in window)
            ~ check_rate_limit("alice");

            // Exhaust the limit
            for _ in 0..9 {
                ~ check_rate_limit("alice");
            }

            // Advance past the window
            // (clock advance happens outside the effect, so we fork here)
        });

        let handle = eff.fork();
        clock.advance(Duration::from_secs(61));
        handle.join()
    });
}
```

`run_test_with_clock_and_env` combines both: a controlled clock and a custom service environment.
