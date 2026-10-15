//! Scheduling: the immutable `Schedule<Out, In>` with its combinators (`recurs`, `spaced`,
//! `exponential`, `fibonacci`, `jittered`, `union`, `intersect`, `and_then`, `while_input`,
//! `up_to`) and the `retry` / `repeat` associated functions on `Effect` (no free functions).
//! Filled by `p-schedule`.
//!
//! Delays come from the executor clock, so they run in virtual time on `TestExecutor`; durations
//! are `std::time::Duration` at the boundary.

mod schedule;

pub use schedule::*;
