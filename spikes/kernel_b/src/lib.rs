//! Kernel spike B: typed `R` as a trait-bound environment (`Has<T>`, `#[derive(Env)]`).
//!
//! Plan: `.maestro/specs/0.7/kernel-design-b-traitenv.md` (a standalone crate per ruling R1,
//! not `kernel_v2` in core) plus the R1 shared ideas: an inline `Ready` step, `Cause<E>` as the
//! internal error channel, a one-shot exit slot, and `effect_local!`/`LocalEffect`. Findings are
//! in `SPIKE-NOTES.md`.
//!
//! Map:
//! - [`kernel`]: `Effect` / `LocalEffect` (one `kernel_impl!` template), constructors, binds.
//! - [`env`]: `Has`, `Replace`, `Env`, `Service`, `IntoBind`, `caps!`.
//! - [`fiber`]: fibers, fork/join/interrupt, masks, finalizers, `sleep`.
//! - [`executor`]: `Executor`, `ParkExecutor` (`block_on`), tokio backend, `Runtime`.
//! - [`par`]: `zip_par`, `race`, `timeout`.
//! - [`layer`]: a minimal `Layer`.

extern crate self as kernel_b;

pub mod cause;
pub mod env;
pub mod executor;
pub mod fiber;
pub mod kernel;
pub mod layer;
pub mod par;

pub use cause::{Cause, Defect, Exit, Never};
pub use env::{__bind, __widen, Env, Has, IntoBind, Replace, Service, need};
#[cfg(feature = "tokio")]
pub use executor::TokioExecutor;
pub use executor::{Executor, ParkExecutor, Runtime, block_on, block_on_local, run_blocking};
pub use fiber::{FiberHandle, FiberId, acquire_use_release, in_fiber, sleep};
pub use kernel::{
  BoxFuture, Effect, LocalBoxFuture, LocalEffect, box_future, box_local_future, fail, from_async,
  succeed, yield_now,
};
pub use kernel_b_derive::{Env, effect, effect_local};
pub use layer::Layer;
pub use par::{race, zip_par};
