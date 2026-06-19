//! **Stratum 4 — Failure Algebra**
//!
//! Structured representation of computational failure, built entirely from Strata 0–3.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`cause`] | [`Cause<E>`] ADT, [`Semigroup`] impl | Stratum 0 (`option_`, `Matcher`), Stratum 1 (`Semigroup`), Stratum 6 bootstrap (`FiberId`) |
//! | [`exit`]  | [`Exit<A,E>`] terminal outcome | [`cause`] (this stratum), Stratum 0 (`Either`, `Matcher`) |
//! | [`union`] | [`Or<L,R>`] error union | Stratum 0 (`Either`) |
//!
//! ## Public API
//!
//! Re-exported at the crate root: [`Cause`], [`Exit`], [`Or`].

pub mod cause;
pub mod exit;
pub mod pretty;
pub mod union;

pub use cause::Cause;
pub use exit::Exit;
pub use pretty::{pretty_cause, pretty_cause_inline, pretty_exit, pretty_fiber_id};
pub use union::Or;
