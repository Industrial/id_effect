//! Ex 020 — admission control: a token-bucket `RateLimiter` and a fail-fast `Bulkhead`.
//!
//! The limiter reads time from a `TestClock`, so refill is driven by `advance` and the example is
//! deterministic.
//!
//! Run: `cargo run -p id_effect_resilience --example 020_rate_limit`

use std::time::{Duration, Instant};

use id_effect::failure::Or;
use id_effect::kernel::{Effect, box_future, succeed};
use id_effect::runtime::{run_async, run_blocking};
use id_effect::scheduling::TestClock;
use id_effect_resilience::{Bulkhead, BulkheadError, RateLimitError, RateLimiter};

fn main() {
  // 2 requests/second with a burst of 3.
  let clock = TestClock::new(Instant::now());
  let limiter =
    run_blocking(RateLimiter::make_with_clock(2.0, 3.0, clock.clone()), ()).expect("limiter");

  let burst: Vec<bool> = (0..4)
    .map(|_| run_blocking(limiter.acquire(), ()).is_ok())
    .collect();
  println!("burst of 4: {burst:?}");
  assert_eq!(burst, [true, true, true, false]);
  assert_eq!(run_blocking(limiter.acquire(), ()), Err(RateLimitError));

  clock.advance(Duration::from_millis(500)); // +1 token
  println!("after 500 ms: {:?}", run_blocking(limiter.acquire(), ()));

  // A bulkhead of 1 rejects (rather than queues) a second concurrent call.
  let bulkhead = run_blocking(Bulkhead::make(1), ()).expect("bulkhead");
  let inner_bulkhead = bulkhead.clone();
  let nested = bulkhead.with_permit(Effect::new_async(move |_r: &mut ()| {
    box_future(async move {
      Ok::<_, ()>(run_async(inner_bulkhead.with_permit(succeed::<u32, (), ()>(1)), ()).await)
    })
  }));
  let inner = run_blocking(nested, ()).expect("outer call admitted");
  println!("second concurrent call: {inner:?}");
  assert_eq!(inner, Err(Or::Left(BulkheadError)));
}
