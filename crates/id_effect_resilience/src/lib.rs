//! Resilience patterns for [`id_effect::Effect`] programs.
//!
//! | Module | Role |
//! |--------|------|
//! | [`circuit_breaker`] | Fail fast after repeated failures |
//! | [`rate_limiter`] | Token-bucket admission control |
//! | [`bulkhead`] | Limit concurrent in-flight effects |
//! | [`hedged()`] | Race primary and backup effects |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod bulkhead;
pub mod circuit_breaker;
pub mod hedged;
pub mod rate_limiter;

pub use bulkhead::{Bulkhead, BulkheadError};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerError, CircuitState};
pub use hedged::hedged;
pub use rate_limiter::{RateLimitError, RateLimiter};
