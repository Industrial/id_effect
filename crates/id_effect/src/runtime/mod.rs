//! **Stratum 6 — Execution & Runtime**
//!
//! The interpreter that brings effects to life, built from Strata 0–5.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`execute`] | [`run_blocking`], [`run_async`], [`Never`] | Stratum 2 (`kernel::Effect`, `kernel::BoxFuture`) |
//! | [`rt`] | [`Runtime`], [`ThreadSleepRuntime`], [`run_fork`], [`yield_now`] | [`execute`] (this stratum), Stratum 7 (`concurrency::{FiberHandle, FiberId}`), Stratum 2 (`kernel::Effect`) |
//!
//! Fiber and cancellation types ([`FiberId`], [`FiberHandle`], [`FiberStatus`],
//! [`CancellationToken`]) live in Stratum 7 ([`crate::concurrency`]); they are re-exported here
//! so existing `crate::runtime::FiberId` paths continue to resolve.
//!
//! ## Design
//!
//! ```text
//! run_blocking[A, E, R]: Effect[A, E, R] → R → Result[A, E]   -- synchronous driver
//! run_async[A, E, R]:    Effect[A, E, R] → R → Future[…]       -- async driver
//! run_fork[A, E, R]:     Runtime → (() → (Effect, R)) → FiberHandle[A, E]   -- Send factory; effect built on worker
//! ```

pub mod execute;
pub mod rt;

pub use execute::{Never, run_async, run_blocking};
pub use rt::{Runtime, ThreadSleepRuntime, run_fork, yield_now};

// Re-export Stratum 7 concurrency types so `crate::runtime::FiberId` etc. keep compiling.
pub use crate::concurrency::{
  CancellationToken, FiberHandle, FiberId, FiberStatus, check_interrupt, fiber_all, fiber_never,
  fiber_succeed, interrupt_all,
};
