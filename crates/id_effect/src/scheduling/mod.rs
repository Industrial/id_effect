//! **Scheduling & time.**
//!
//! | Submodule | Provides |
//! |-----------|----------|
//! | [`duration`] | [`Duration`], [`DurationParseError`] |
//! | [`datetime`] | [`UtcDateTime`], [`ZonedDateTime`], [`AnyDateTime`], [`TimeUnit`] (`jiff`) |
//! | `schedule` | the immutable [`Schedule<Out, In>`](Schedule) with its combinators (`recurs`, `spaced`, `exponential`, `fibonacci`, `jittered`, `union`, `intersect`, `and_then`, `while_input`, `up_to`) and the `retry` / `repeat` associated functions on `Effect` (no free functions) |
//!
//! Schedule delays come from the executor clock, so they run in virtual time on
//! `TestExecutor`; schedule durations are `std::time::Duration` at the boundary.
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod datetime;
pub mod duration;
mod schedule;

pub use datetime::{AnyDateTime, TimeUnit, UtcDateTime, ZonedDateTime, timezone};
pub use duration::{Duration, DurationParseError};
pub use schedule::*;
