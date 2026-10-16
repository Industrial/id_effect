# Fallbacks and Resilience Pipelines

A single policy rarely protects a call on its own. A request to a flaky price service wants a
timeout so it cannot hang, a retry for the blips, a circuit breaker so an outage does not turn
into a retry storm, and — when all of that fails — a stale cached answer instead of an error
page. `id_effect_resilience` provides each policy separately (`CircuitBreaker`, `RateLimiter`,
`Bulkhead`, `hedged`, `retry_with_breaker`) and two pieces that tie them together: `Fallback` and
`Pipeline`.

## Fallback

A `Fallback` is a reusable "answer with this instead". `Fallback::value(v)` answers with a copy
of `v`; `Fallback::new(|err| effect)` runs another effect (a cache, a secondary source), which may
itself fail. `when(pred)` restricts it to the failures it should handle — everything else, and
every defect or interruption, passes through untouched:

```rust
use id_effect::{Effect, Exit, fail};
use id_effect_resilience::Fallback;

let cached = Fallback::value("cached profile").when(|e: &&str| *e == "timeout");

let slow: Effect<&str, &str> = fail("timeout");
assert_eq!(cached.apply(slow).block_on(), Exit::Success("cached profile"));

let denied: Effect<&str, &str> = fail("forbidden");
assert_eq!(cached.apply(denied).block_on(), Exit::fail("forbidden"));
```

## Pipeline: One Policy Value

A `Pipeline` composes the policies into one value you build once and apply to every call. Add
only the policies you need; they always nest in the same order, outermost first:

1. **fallback** — recovers from the errors it handles;
2. **retry** — a `Schedule` re-running the inner layers while the error is transient (a failure
   or a timeout); a rejection (open breaker, full bulkhead, no rate-limit token) ends it at once;
3. **rate limiter** — one token per attempt;
4. **bulkhead** — at most N attempts in flight;
5. **circuit breaker** — counts failures and timeouts, rejects while open;
6. **timeout** — per attempt;
7. the call.

```rust
use std::time::Duration;
use id_effect::{Exit, Schedule, fail, succeed};
use id_effect_resilience::{CircuitBreaker, Fallback, Pipeline, PipelineError};

let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
let prices: Pipeline<u32, &str> = Pipeline::new("prices")
    .timeout(Duration::from_millis(200))
    .retry(Schedule::exponential(Duration::from_millis(1)).intersect(Schedule::recurs(3)))
    .circuit_breaker(&breaker)
    .fallback(Fallback::value(40).when(|e: &PipelineError<&str>| {
        matches!(e, PipelineError::Failed(_) | PipelineError::CircuitOpen)
    }));

assert_eq!(prices.execute(|| succeed(42)).block_on(), Exit::Success(42));
// Three failures open the breaker, the fourth attempt is rejected, the fallback answers.
assert_eq!(prices.execute(|| fail("503")).block_on(), Exit::Success(40));
```

`execute` takes a factory (`Fn() -> Effect`) because an effect runs once and a retried call must
be rebuilt for each attempt. Errors come back as `PipelineError<E>`: `Failed(e)` for the call's
own error, or `Timeout`, `CircuitOpen`, `RateLimited`, `BulkheadFull` for the policy that
stopped it. Clones of a pipeline share its breaker, bulkhead and rate limiter, so one pipeline
per dependency is the usual shape.

## Telemetry

A pipeline reports what it does as `PipelineEvent`s — `Attempt`, `Success`, `Failure` (with the
error kind) and `Fallback`:

- `on_event(|name, event| ..)` adds a listener, for logs or tests;
- `metrics(&frequency)` counts each event's label (`attempt`, `success`, `failure.timeout`,
  `fallback.circuit_open`, …) in an `id_effect` `Frequency` metric tagged `pipeline=<name>`, so
  it is exported with the rest of the metric registry (for example on the Prometheus endpoint).

```rust,ignore
let events = Frequency::new("resilience_events");
let prices = Pipeline::new("prices")
    .retry(Schedule::recurs(3))
    .on_event(|name, event| tracing::debug!(pipeline = name, ?event))
    .metrics(&events);
```

## Testing on Virtual Time

Timeouts, backoff delays, breaker cooldowns and token refills all run on the fiber's executor
clock, so under a `TestExecutor` a scenario like "time out after one second, back off, fail, back
off, succeed" completes instantly and you can assert the exact virtual time it took. The
crate's `tests/pipeline.rs` does exactly that, and `examples/030_resilience_pipeline.rs` runs the
price-service scenario above with its event log and metric counts:

```text
cargo run -p id_effect_resilience --example 030_resilience_pipeline
```
