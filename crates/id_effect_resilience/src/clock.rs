//! Time source shared by the stateful primitives (breaker cooldown, token refill).

use std::sync::Arc;
use std::time::Instant;

use id_effect::scheduling::Clock;

/// Monotonic "now" used for cooldown and refill arithmetic.
pub(crate) type NowFn = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Wall-clock time source ([`Instant::now`]).
pub(crate) fn system_now() -> NowFn {
  Arc::new(Instant::now)
}

/// Read time from `clock` (e.g. a [`TestClock`](id_effect::scheduling::TestClock) in tests).
pub(crate) fn clock_now<C>(clock: C) -> NowFn
where
  C: Clock + Send + Sync + 'static,
{
  Arc::new(move || clock.now())
}
