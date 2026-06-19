//! **Stratum 16 — Testing**
//!
//! Deterministic test harness utilities, built from all lower strata.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`snapshot`] | `SnapshotAssertion` builders | Most strata (integration) |
//! | [`test_runtime`] | [`run_test`], [`run_test_with_clock`], leak/scope checkers | Stratum 7 (`concurrency`), Stratum 8 (`resource`), Stratum 10 (`scheduling`) |

pub mod proptest;
pub mod snapshot;
pub mod test_runtime;

#[cfg(feature = "proptest")]
pub use proptest::{
  arb_exit_fail, arb_exit_success, prop_assert_exit_eq, prop_assert_exit_success, success_value,
};
pub use proptest::{assert_exit_eq, assert_exit_success, exit_success_value, run_effect};
pub use snapshot::{
  GoldenBuilder, SnapshotAssertion, assert_golden, assert_golden_effect, assert_golden_matches,
};
pub use test_runtime::{
  assert_no_leaked_fibers, assert_no_unclosed_scopes, record_leaked_fiber, record_unclosed_scope,
  run_test, run_test_with_clock,
};
