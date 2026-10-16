//! **Scheduling & time.**
//!
//! | Submodule | Provides |
//! |-----------|----------|
//! | [`duration`] | [`Duration`], [`DurationParseError`] |
//! | [`datetime`] | [`UtcDateTime`], [`ZonedDateTime`], [`AnyDateTime`], [`TimeUnit`] (`jiff`) |
//! | `schedule` | the immutable [`Schedule<Out, In>`](Schedule) with its combinators (`recurs`, `spaced`, `exponential`, `fibonacci`, `jittered`, `union`, `intersect`, `and_then`, `while_input`, `up_to`) and the `retry` / `repeat` associated functions on `Effect` (no free functions) |
//! | [`cron`] | [`Cron`] expressions with zone- and DST-aware [`Cron::next_after`], the [`WallClock`] mapping the executor clock to civil time, and [`Schedule::cron`] |
//! | [`cron_job`] | named [`CronJob`]s with [`OverlapPolicy`] / [`MissedRunPolicy`], success / failure hooks and run history ([`CronRun`]), registered in a [`CronRegistry`] and observed through a [`CronScheduler`] |
//!
//! Schedule delays come from the executor clock, so they run in virtual time on
//! `TestExecutor`; schedule durations are `std::time::Duration` at the boundary.
//!
//! ## Public API
//!
//! Re-exported at the crate root: all public types and functions.

pub mod cron;
pub mod cron_job;
pub mod datetime;
pub mod duration;
mod schedule;

pub use cron::{Cron, CronParseError, WallClock};
pub use cron_job::{
  CronError, CronJob, CronOutcome, CronRegistry, CronRun, CronScheduler, CronSkip, MissedRunPolicy,
  OverlapPolicy,
};
pub use datetime::{AnyDateTime, TimeUnit, UtcDateTime, ZonedDateTime, timezone};
pub use duration::{Duration, DurationParseError};
pub use schedule::*;
