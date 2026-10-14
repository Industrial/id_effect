//! Tracing span helpers for AI vendor calls.

use id_effect::kernel::Effect;
use id_effect::with_span;

use crate::error::AiError;

/// Wrap an AI vendor effect in a tracing span (no prompt content).
pub fn with_ai_request_span<A, E, R>(
  vendor: &'static str,
  operation: &'static str,
  model: &str,
  effect: Effect<A, E, R>,
) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Send + 'static,
{
  let _ = (vendor, operation, model);
  with_span(effect, "ai.request")
}

/// Same as [`with_ai_request_span`] for [`AiError`] programs on `()`.
pub fn with_ai_span<A>(
  vendor: &'static str,
  operation: &'static str,
  model: &str,
  effect: Effect<A, AiError, ()>,
) -> Effect<A, AiError, ()>
where
  A: Send + 'static,
{
  with_ai_request_span(vendor, operation, model, effect)
}
