//! **Stratum 16 — Testing**
//!
//! Deterministic test harness utilities, built from all lower strata.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`snapshot`] | `SnapshotAssertion` builders | Most strata (integration) |
//! | [`test_runtime`] | [`run_test`], [`run_test_with_clock`], leak/scope checkers | Stratum 7 (`concurrency`), Stratum 8 (`resource`), Stratum 10 (`scheduling`) |

pub mod snapshot;
pub mod test_runtime;

pub use snapshot::SnapshotAssertion;
pub use test_runtime::{
  assert_no_leaked_fibers, assert_no_unclosed_scopes, record_leaked_fiber, record_unclosed_scope,
  run_test, run_test_with_clock,
};
