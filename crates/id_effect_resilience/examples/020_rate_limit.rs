//! Ex 020 — admission control: a token-bucket `RateLimiter`, a fail-fast `Bulkhead` and a
//! `hedged` call.
//!
//! Run: `cargo run -p id_effect_resilience --example 020_rate_limit`

use std::time::Duration;

use id_effect::next::{Effect, sleep, succeed, zip_par};
use id_effect::{Exit, Or};
use id_effect_resilience::{Bulkhead, BulkheadError, RateLimitError, RateLimiter, hedged};

fn main() {
  // 20 requests/second with a burst of 3.
  let limiter = RateLimiter::new(20.0, 3.0);
  let take = || -> Effect<bool, RateLimitError> {
    limiter.acquire().map(|()| true).catch(|_| succeed(false))
  };
  let burst = take()
    .zip(take())
    .zip(take())
    .zip(take())
    .map(|(((a, b), c), d)| [a, b, c, d]);
  let Exit::Success(burst) = burst.block_on() else {
    panic!("burst")
  };
  println!("burst of 4: {burst:?}");
  assert_eq!(burst, [true, true, true, false]);

  // 50 ms at 20/s refills one token.
  let refilled = sleep(Duration::from_millis(50)).then(limiter.acquire());
  println!("after 50 ms: {:?}", refilled.block_on());

  // A bulkhead of 1 rejects (rather than queues) a second concurrent call.
  let bulkhead = Bulkhead::new(1);
  let call = || {
    bulkhead
      .with_permit(sleep::<(), _>(Duration::from_millis(10)).map(|()| "done"))
      .map(Ok)
      .catch(|e: Or<BulkheadError, ()>| succeed::<_, (), _>(Err(e)))
  };
  let Exit::Success((a, b)) = zip_par(call(), call()).block_on() else {
    panic!("bulkhead")
  };
  println!("two concurrent calls: {a:?}, {b:?}");
  assert!(a.is_err() ^ b.is_err());

  // Hedge a slow primary with a backup started after 5 ms: the backup wins.
  let primary: Effect<&str> = sleep(Duration::from_secs(3600)).map(|()| "primary");
  let out = hedged(primary, succeed("backup"), Duration::from_millis(5)).block_on();
  println!("hedged: {out:?}");
  assert_eq!(out, Exit::Success("backup"));
}
