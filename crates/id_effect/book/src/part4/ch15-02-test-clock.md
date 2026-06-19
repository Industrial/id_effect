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
use id_effect::{run_test_with_clock, TestClock, Exit};

#[test]
fn retry_exhaustion_fast() {
    let clock = TestClock::new();
    let eff = failing_call()
        .retry(Schedule::exponential(Duration::from_secs(1)).take(3));

    // run_test_with_clock runs the effect with the supplied clock handle
    let exit = run_test_with_clock(eff, (), clock.clone());

    assert!(matches!(exit, Exit::Failure(_)));
}
```

`run_test_with_clock(effect, env, clock)` runs an effect in the deterministic test harness with an explicit `TestClock`. Inject a clock capability via your provider graph when the effect reads time from `Env`.

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
    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();

    let job = effect!(|_r: &mut ()| {
        c.fetch_add(1, Ordering::Relaxed);
    })
    .repeat(Schedule::fixed(Duration::from_secs(60)));

    let clock = TestClock::new();
    let _handle = job.fork();

    // Advance through 3 minutes
    clock.advance(Duration::from_secs(60));
    clock.advance(Duration::from_secs(60));
    clock.advance(Duration::from_secs(60));

    assert_eq!(counter.load(Ordering::Relaxed), 3);
}
```

## Time and Race Conditions

`TestClock` is deterministic — time moves only when you call `advance`. This means tests that use `TestClock` have no time-based race conditions: the scheduler runs wake-up callbacks synchronously when you advance.

If your effect spawns multiple fibers that all sleep, advancing time wakes all fibers whose sleep deadline has passed, in a consistent order.

## Combining TestClock with Fake Services

```rust
#[::id_effect::capability(Arc<dyn RateLimitStore>)]
struct RateLimitStoreCap;

mock_capability!(MockRateLimitStore, RateLimitStoreCapKey, Arc<dyn RateLimitStore>, "ratelimit/mock", || {
    Arc::new(InMemoryRateLimitStore::new()) as Arc<dyn RateLimitStore>
});

#[test]
fn rate_limiter_enforces_window() {
    let env = build_env([provide!(MockRateLimitStore)]).expect("env");
    let clock = TestClock::new();

    let eff = check_rate_limit_flow("alice");
    let exit = run_test_with_clock(eff, env, clock.clone());

    assert!(matches!(exit, Exit::Success(_)));
}
```

Build the capability env with `build_env([provide!(…), …])`, then pass it to `run_test_with_clock` alongside your `TestClock`.
