//! [`StoreFailure`]: how a store call run under the [`RequestPolicy`](super::RequestPolicy) fails.

use id_effect::err;
use id_effect_resilience::CircuitBreakerError;

use super::RequestTimeout;
use crate::domain::todo::StoreError;

/// A failed store call: it timed out ([`RequestTimeout`]), the circuit breaker is open
/// ([`CircuitBreakerError`]), or the store failed after the retries ([`StoreError`]).
pub type StoreFailure = err!(RequestTimeout | CircuitBreakerError | StoreError);
