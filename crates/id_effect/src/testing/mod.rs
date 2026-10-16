//! Test support for the 0.7 kernel: the deterministic `TestExecutor`.

mod test_executor;
mod test_random;

pub use test_executor::*;
pub use test_random::*;

// Exit/Cause assertions, `run_test`, golden files; the `proptest` feature adds `property`.
mod assert;
mod golden;
#[cfg(feature = "proptest")]
pub mod property;

pub use assert::*;
pub use golden::*;
