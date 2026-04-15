//! **Stratum 10 — Scheduling & Time**
//!
//! Policies for repetition, retry, and temporal reasoning, built from Strata 0–9.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`duration`] | [`Duration`], [`DurationParseError`] | Stratum 0 |
//! | [`datetime`] | [`UtcDateTime`], [`ZonedDateTime`], [`AnyDateTime`], [`TimeUnit`] | [`duration`], `jiff` |
//! | [`clock`] | [`Clock`], [`LiveClock`], [`TestClock`] | [`datetime`], Stratum 6 (`runtime`) |
//! | [`schedule`] | [`Schedule`], repeat/retry combinators | [`clock`], [`duration`], Stratum 6 |
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod clock;
pub mod datetime;
pub mod duration;
pub mod schedule;

pub use clock::{Clock, LiveClock, TestClock};
pub use datetime::{AnyDateTime, TimeUnit, UtcDateTime, ZonedDateTime, timezone};
pub use duration::{Duration, DurationParseError};
pub use schedule::{
  Schedule, ScheduleDecision, ScheduleInput, forever, repeat, repeat_n, repeat_with_clock,
  repeat_with_clock_and_interrupt, retry, retry_with_clock, retry_with_clock_and_interrupt,
};
