//! Time source shared by the stateful primitives (breaker cooldown, token refill).

use std::future::ready;
use std::time::Instant;

use id_effect::{Ctx, Effect};

/// The running fiber's executor clock: wall-clock time on `TokioExecutor`, virtual time on
/// `TestExecutor` (so cooldowns and refills follow `TestExecutor::advance`).
pub(crate) fn now<E, R>() -> Effect<Instant, E, R>
where
  E: Send + 'static,
  R: Send + Sync + 'static,
{
  Effect::from_ctx(|ctx: Ctx<R>| ready(Ok(ctx.fiber().executor().now())))
}
