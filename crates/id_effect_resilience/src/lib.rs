//! Resilience patterns for [`id_effect::Effect`] programs.
//!
//! | Module | Role |
//! |--------|------|
//! | [`circuit_breaker`] | Fail fast after repeated failures |
//! | [`rate_limiter`] | Token-bucket admission control |
//! | [`bulkhead`] | Limit concurrent in-flight effects (rejects when full) |
//! | [`hedged()`] | Race primary and backup effects |
//! | [`retry`] | Retry per a core [`Schedule`](id_effect::scheduling::Schedule) through a breaker |
//!
//! ```
//! use std::time::Duration;
//! use id_effect::runtime::run_blocking;
//! use id_effect::scheduling::Schedule;
//! use id_effect::kernel::fail;
//! use id_effect::failure::Or;
//! use id_effect_resilience::{CircuitBreaker, CircuitBreakerError, retry_with_breaker};
//!
//! let breaker = run_blocking(CircuitBreaker::make(2, Duration::from_secs(30)), ()).unwrap();
//! // Retry up to 5 times, but the breaker opens after 2 failures and stops the loop.
//! let out = run_blocking(
//!   retry_with_breaker(|| fail::<u32, &str, ()>("down"), Schedule::recurs(5), &breaker),
//!   (),
//! );
//! assert!(matches!(out, Err(Or::Left(CircuitBreakerError))));
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod bulkhead;
pub mod circuit_breaker;
mod clock;
pub mod hedged;
pub mod rate_limiter;
pub mod retry;

pub use bulkhead::{Bulkhead, BulkheadError};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerError, CircuitState};
pub use hedged::hedged;
pub use rate_limiter::{RateLimitError, RateLimiter};
pub use retry::{retry_with_breaker, retry_with_breaker_and_clock};
