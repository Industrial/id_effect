//! Ex 030 — one reusable `Pipeline`: timeout, retry, circuit breaker and a fallback, with
//! telemetry (an event listener and a `Frequency` metric).
//!
//! Delays are a few milliseconds of real time here; on a `TestExecutor` the same program runs on
//! virtual time (see `tests/pipeline.rs`).
//!
//! Run: `cargo run -p id_effect_resilience --example 030_resilience_pipeline`

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use id_effect::{Effect, Exit, Frequency, Metric, Schedule, fail, sleep, succeed, suspend};
use id_effect_resilience::{CircuitBreaker, Fallback, Pipeline, PipelineError};

type Price = Effect<u32, &'static str>;

/// A price service: the first `slow` calls hang for a second, the next `failing` fail with 503,
/// the rest answer 42.
fn price_service(slow: u32, failing: u32) -> impl Fn() -> Price + Send + Sync + use<> {
  let calls = Arc::new(AtomicU32::new(0));
  move || {
    let calls = Arc::clone(&calls);
    suspend(move || {
      let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
      if n <= slow {
        sleep(Duration::from_secs(1)).map(|()| 0)
      } else if n <= slow + failing {
        fail("503")
      } else {
        succeed(42)
      }
    })
  }
}

fn main() {
  let events = Frequency::new("example_resilience_events");
  let breaker = CircuitBreaker::new(3, Duration::from_secs(30));
  let pipeline: Pipeline<u32, &str> = Pipeline::new("prices")
    .timeout(Duration::from_millis(20))
    .retry(Schedule::exponential(Duration::from_millis(1)).intersect(Schedule::recurs(3)))
    .circuit_breaker(&breaker)
    // Serve a stale price when the service is down; let other rejections through.
    .fallback(Fallback::value(40).when(|e: &PipelineError<&str>| {
      matches!(e, PipelineError::Failed(_) | PipelineError::CircuitOpen)
    }))
    .on_event(|name, event| println!("  [{name}] {event:?}"))
    .metrics(&events);

  println!("1. a slow call times out, then a failure, then success on attempt 3:");
  let out = pipeline.execute(price_service(1, 1)).block_on();
  println!("   -> {out:?}");
  assert_eq!(out, Exit::Success(42));

  println!("2. an outage opens the breaker; the fallback serves the stale price:");
  let out = pipeline.execute(price_service(0, u32::MAX)).block_on();
  println!("   -> {out:?}");
  assert_eq!(out, Exit::Success(40));

  println!("3. while the breaker is open, calls are rejected at once (fallback again):");
  let out = pipeline.execute(price_service(0, 0)).block_on();
  println!("   -> {out:?}");
  assert_eq!(out, Exit::Success(40));

  println!("telemetry (pipeline=prices):");
  for (label, n) in events.tagged("pipeline", "prices").occurrences() {
    println!("  {label}: {n}");
  }
}
